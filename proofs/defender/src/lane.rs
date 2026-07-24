use crate::{
    traits::DefenderClient,
    types::{GameMetadata, LaneState},
};
use alloy_primitives::Bytes;
use tracing::{error, info, warn};
use world_chain_proofs::ProofLane;
use world_chain_prover_service::{
    ProofBackend, ProofData, ProofRequest, ProofRequestId, ProofRequester, ProofResponse,
    ProofStatus,
};

pub(crate) struct LaneDriver<'a, E, P> {
    execution_client: &'a E,
    proof_requester: &'a P,
    max_proof_attempts: u32,
}

impl<'a, E, P> LaneDriver<'a, E, P>
where
    E: DefenderClient,
    P: ProofRequester + Sync,
{
    pub(crate) const fn new(
        execution_client: &'a E,
        proof_requester: &'a P,
        max_proof_attempts: u32,
    ) -> Self {
        Self {
            execution_client,
            proof_requester,
            max_proof_attempts,
        }
    }

    pub(crate) async fn advance(
        &self,
        metadata: &GameMetadata,
        lane: ProofLane,
        backend: ProofBackend,
        state: LaneState,
    ) -> LaneState {
        match state {
            LaneState::Proven | LaneState::Abandoned => state,
            LaneState::Pending => self.request_pending_lane(metadata, lane, backend).await,
            LaneState::Requested { id, attempts } => {
                self.advance_requested_lane(metadata, lane, backend, id, attempts)
                    .await
            }
        }
    }

    async fn request_pending_lane(
        &self,
        metadata: &GameMetadata,
        lane: ProofLane,
        backend: ProofBackend,
    ) -> LaneState {
        let game = metadata.address;
        match self
            .proof_requester
            .request_proof(proof_request(metadata, backend))
            .await
        {
            Ok(id) => LaneState::Requested { id, attempts: 1 },
            Err(error) => {
                warn!(%game, ?lane, %error, "proof request failed; retrying next tick");
                LaneState::Pending
            }
        }
    }

    async fn advance_requested_lane(
        &self,
        metadata: &GameMetadata,
        lane: ProofLane,
        backend: ProofBackend,
        id: ProofRequestId,
        attempts: u32,
    ) -> LaneState {
        let game = metadata.address;
        let state = LaneState::Requested { id, attempts };
        let status = match self.proof_requester.proof_status(id).await {
            Ok(status) => status,
            Err(error) => {
                warn!(%game, ?lane, %id, %error, "proof status check failed; retrying next tick");
                return state;
            }
        };

        match status {
            ProofStatus::Created | ProofStatus::Running => state,
            ProofStatus::Succeeded => self.submit_succeeded_lane(metadata, lane, id, state).await,
            ProofStatus::Failed => {
                self.retry_failed_lane(metadata, lane, backend, attempts, state)
                    .await
            }
        }
    }

    async fn submit_succeeded_lane(
        &self,
        metadata: &GameMetadata,
        lane: ProofLane,
        id: ProofRequestId,
        state: LaneState,
    ) -> LaneState {
        let game = metadata.address;
        let response = match self.proof_requester.get_proof(id).await {
            Ok(ProofResponse::Succeeded(response)) => response,
            Ok(ProofResponse::Pending(response)) => {
                warn!(
                    %game,
                    ?lane,
                    %id,
                    status = %response.status,
                    "proof status was succeeded but proof response is pending; retrying next tick"
                );
                return state;
            }
            Ok(ProofResponse::Failed(response)) => {
                warn!(
                    %game,
                    ?lane,
                    %id,
                    reason = %response.reason,
                    "proof status was succeeded but proof response is failed; retrying next tick"
                );
                return state;
            }
            Err(error) => {
                warn!(%game, ?lane, %id, %error, "proof retrieval failed; retrying next tick");
                return state;
            }
        };

        match self
            .execution_client
            .submit_proof(game, lane as u8, encode_proof(&response.proof))
            .await
        {
            Ok(submission) => {
                info!(%game, ?lane, tx_hash = %submission.tx_hash, "proof lane submitted");
                LaneState::Proven
            }
            Err(error) => {
                // if the transaction actually landed, the proof bitmap check
                // resolves the lane on the next tick
                warn!(%game, ?lane, %error, "proof submission failed; retrying next tick");
                state
            }
        }
    }

    async fn retry_failed_lane(
        &self,
        metadata: &GameMetadata,
        lane: ProofLane,
        backend: ProofBackend,
        attempts: u32,
        state: LaneState,
    ) -> LaneState {
        let game = metadata.address;
        if attempts >= self.max_proof_attempts {
            error!(%game, ?lane, attempts, "proving permanently failed; abandoning lane");
            return LaneState::Abandoned;
        }

        // re-requesting a failed proof re-queues it
        match self
            .proof_requester
            .request_proof(proof_request(metadata, backend))
            .await
        {
            Ok(id) => {
                let next_attempt = attempts + 1;
                warn!(
                    %game,
                    ?lane,
                    %id,
                    attempts = next_attempt,
                    max_attempts = self.max_proof_attempts,
                    "proof failed; re-requested proof"
                );
                LaneState::Requested {
                    id,
                    attempts: next_attempt,
                }
            }
            Err(error) => {
                warn!(%game, ?lane, %error, "proof re-request failed; retrying next tick");
                state
            }
        }
    }
}

/// Builds the proof request for one lane of a defended game.
fn proof_request(game: &GameMetadata, backend: ProofBackend) -> ProofRequest {
    ProofRequest {
        backend,
        game: game.address,
        root_claim: game.root_claim,
        l2_block_number: game.l2_block_number,
        // pin the witness to the L1 origin committed at proposal time, so
        // the request id stays stable across defender restarts
        l1_head: game.l1_origin_hash,
    }
}

/// Encode a proof payload into the `bytes` argument of `submitProofLane`.
///
/// TODO: encode proofs for their concrete on-chain verifiers. SP1 proofs must
/// match `SP1ValidityVerifier`'s ABI tuple:
/// `(domainHash, parentRef, l1OriginNumber, publicValues, proofBytes)`.
/// That requires proposal context in addition to `ProofData`, so this helper
/// should move closer to the game/lane submission path before real SP1 lanes
/// are enabled.
fn encode_proof(proof: &ProofData) -> Bytes {
    match proof {
        ProofData::Sp1 {
            proof,
            public_values,
        } => [public_values.as_ref(), proof.as_ref()].concat().into(),
        ProofData::Nitro {
            attestation,
            public_values,
            signature,
        } => [
            public_values.as_ref(),
            attestation.as_ref(),
            signature.as_ref(),
        ]
        .concat()
        .into(),
    }
}
