use crate::{error::DefenderError, traits::DefenderClient, types::GameMetadata};
use alloy_primitives::{Bytes, U256};
use alloy_sol_types::SolValue;
use tracing::{error, info, warn};
use world_chain_proof_core::boot::TransitionPublicValues;
use world_chain_proofs::ProofLane;
use world_chain_prover_service::{
    ProofBackend, ProofData, ProofRequest, ProofRequestError, ProofRequestId, ProofRequester,
    ProofResponse, ProofStatus,
};

/// Number of proof lanes the defender drives.
pub(crate) const DEFENDED_LANE_COUNT: usize = 2;

/// The proof lanes the defender drives, paired with the prover-service
/// backend that generates each proof.
pub(crate) const DEFENDED_LANES: [(ProofLane, ProofBackend); DEFENDED_LANE_COUNT] = [
    (ProofLane::ValidityProof, ProofBackend::Sp1),
    (ProofLane::TeeAttestation, ProofBackend::Nitro),
];

/// Progress of a single proof lane within an active defense.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaneState {
    /// The proof has not been requested yet.
    Pending,
    /// The proof was requested from the prover-service.
    Requested { id: ProofRequestId },
    /// The lane is proven on-chain.
    Proven,
    /// Proving permanently failed after exhausting all attempts.
    Abandoned,
}

impl LaneState {
    /// Whether the lane needs no further work.
    pub(crate) const fn is_terminal(self) -> bool {
        matches!(self, Self::Proven | Self::Abandoned)
    }
}

pub(crate) struct LaneDriver<'a, E, P> {
    execution_client: &'a E,
    proof_requester: &'a P,
}

impl<'a, E, P> LaneDriver<'a, E, P>
where
    E: DefenderClient,
    P: ProofRequester + Sync,
{
    pub(crate) const fn new(execution_client: &'a E, proof_requester: &'a P) -> Self {
        Self {
            execution_client,
            proof_requester,
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
            LaneState::Requested { id } => {
                self.advance_requested_lane(metadata, lane, backend, id)
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
            Ok(request_proof_response) => LaneState::Requested {
                id: request_proof_response.proof_id,
            },
            Err(ProofRequestError::TooManyRetries(error)) => {
                error!(%game, ?lane, %error, "prover-service exhausted retries; abandoning lane");
                LaneState::Abandoned
            }
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
    ) -> LaneState {
        let game = metadata.address;
        let state = LaneState::Requested { id };
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
            ProofStatus::Failed => self.retry_failed_lane(metadata, lane, backend, state).await,
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
            .submit_proof(
                game,
                lane as u8,
                match encode_proof(metadata, &response.proof) {
                    Ok(proof) => proof,
                    Err(error) => {
                        error!(%game, ?lane, %error, "prover returned an invalid proof payload");
                        return LaneState::Abandoned;
                    }
                },
            )
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
        state: LaneState,
    ) -> LaneState {
        let game = metadata.address;
        // Re-requesting a failed proof re-queues the same deterministic proof id. The
        // prover-service owns the durable retry counter and rejects exhausted requests.
        match self
            .proof_requester
            .request_proof(proof_request(metadata, backend))
            .await
        {
            Ok(request_proof_response) => {
                warn!(
                    %game,
                    ?lane,
                    %request_proof_response.proof_id,
                    "proof failed; re-requested proof"
                );
                LaneState::Requested {
                    id: request_proof_response.proof_id,
                }
            }
            Err(ProofRequestError::TooManyRetries(error)) => {
                error!(%game, ?lane, %error, "prover-service exhausted retries; abandoning lane");
                LaneState::Abandoned
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

/// Encodes the verifier-specific payload passed to `submitProofLane`.
fn encode_proof(metadata: &GameMetadata, proof: &ProofData) -> Result<Bytes, DefenderError> {
    let l1_origin_number = U256::from(metadata.l1_origin_number);
    match proof {
        ProofData::Sp1 {
            proof,
            public_values,
        } => Ok((
            metadata.domain_hash,
            metadata.parent_ref,
            l1_origin_number,
            public_values.clone(),
            proof.clone(),
        )
            .abi_encode()
            .into()),
        ProofData::Nitro {
            attestation: _,
            public_values,
            signature,
        } => {
            // The prover API transports public values as bytes, but NitroProofVerifier embeds
            // TransitionPublicValues as a static tuple. Encoding the bytes directly would add a
            // dynamic ABI offset and produce a payload the verifier cannot decode.
            let transition = <TransitionPublicValues as SolValue>::abi_decode(public_values)
                .map_err(|error| DefenderError::ProofEncoding(error.to_string()))?;
            Ok((
                metadata.domain_hash,
                metadata.parent_ref,
                l1_origin_number,
                transition,
                signature.clone(),
            )
                .abi_encode()
                .into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256};

    #[test]
    fn nitro_encoding_matches_verifier_payload() {
        let transition = TransitionPublicValues {
            l1Head: B256::repeat_byte(0x11),
            l2PreRoot: B256::repeat_byte(0x22),
            l2PreBlockNumber: 10,
            l2PostRoot: B256::repeat_byte(0x33),
            l2PostBlockNumber: 20,
            rollupConfigHash: B256::repeat_byte(0x44),
        };
        let metadata = GameMetadata {
            address: Address::repeat_byte(0x55),
            domain_hash: B256::repeat_byte(0x66),
            parent_ref: Address::repeat_byte(0x77),
            root_claim: transition.l2PostRoot,
            l2_block_number: transition.l2PostBlockNumber,
            l1_origin_hash: transition.l1Head,
            l1_origin_number: 42,
            challenge_deadline: 100,
            proof_deadline: 200,
            proof_threshold: 2,
        };
        let signature = Bytes::from(vec![0x88; 65]);
        let proof = ProofData::Nitro {
            attestation: Bytes::from_static(b"attestation"),
            public_values: transition.abi_encode().into(),
            signature: signature.clone(),
        };

        let encoded = encode_proof(&metadata, &proof).expect("valid Nitro proof payload");
        let expected = (
            metadata.domain_hash,
            metadata.parent_ref,
            U256::from(metadata.l1_origin_number),
            transition,
            signature,
        )
            .abi_encode();

        assert_eq!(encoded.as_ref(), expected);
    }
}
