use crate::{error::DefenderError, traits::DefenderClient, types::GameMetadata};
use alloy_primitives::{Bytes, U256};
use alloy_sol_types::SolValue;
use tracing::{error, info, warn};
use world_chain_proof_core::boot::TransitionPublicValues;
use world_chain_proofs::ProofLane;
use world_chain_prover_service::{
    ProofBackend, ProofData, ProofRequest, ProofRequestId, ProofRequester, ProofResponse,
    ProofStatus,
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
    Requested { id: ProofRequestId, attempts: u32 },
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
            Ok(request_proof_response) => LaneState::Requested {
                id: request_proof_response.proof_id,
                attempts: 1,
            },
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

        let payload = match encode_proof(metadata, &response.proof) {
            Ok(payload) => payload,
            Err(error) => {
                // The prover returned something the lane verifier could never decode. Retrying
                // the same proof cannot help, so surface it loudly rather than burning the
                // remaining proof window on it.
                error!(%game, ?lane, %id, %error, "proof could not be encoded for its lane verifier");
                return LaneState::Abandoned;
            }
        };

        match self
            .execution_client
            .submit_proof(game, lane as u8, payload)
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
            Ok(request_proof_response) => {
                let next_attempt = attempts + 1;
                warn!(
                    %game,
                    ?lane,
                    %request_proof_response.proof_id,
                    attempts = next_attempt,
                    max_attempts = self.max_proof_attempts,
                    "proof failed; re-requested proof"
                );
                LaneState::Requested {
                    id: request_proof_response.proof_id,
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
/// Each lane verifier ABI-decodes a specific tuple, so a bare concatenation of the prover's
/// outputs is rejected as `MALFORMED`. The layouts mirror `SP1ValidityVerifier._decodeAndBind`
/// and `NitroProofVerifier._decodeAndBind`, and match how the proposer builds the creation proof
/// in `world-chain-proposer`'s `submit_proposal`.
fn encode_proof(game: &GameMetadata, proof: &ProofData) -> Result<Bytes, DefenderError> {
    let domain_hash = game.domain_hash;
    let parent_ref = game.parent_ref;
    let l1_origin_number = U256::from(game.l1_origin_number);

    match proof {
        // `(bytes32 domainHash, address parentRef, uint256 l1OriginNumber, bytes publicValues,
        //   bytes proofBytes)` — both tails stay opaque to the defender.
        ProofData::Sp1 {
            proof,
            public_values,
        } => Ok((
            domain_hash,
            parent_ref,
            l1_origin_number,
            public_values.clone(),
            proof.clone(),
        )
            .abi_encode_params()
            .into()),
        // `(bytes32 domainHash, address parentRef, uint256 l1OriginNumber,
        //   TransitionPublicValues transition, bytes signature, bytes expectedPublicKey)`.
        // The transition is a static struct encoded inline, so the prover's pre-encoded
        // `public_values` are decoded and re-encoded in position rather than appended. The
        // attestation document is not part of the payload: the verifier consults the enclave key
        // registry, which the attestation was already used to populate at registration time.
        ProofData::Nitro {
            attestation: _,
            public_values,
            signature,
            public_key,
        } => {
            let transition = TransitionPublicValues::abi_decode(public_values).map_err(|error| {
                DefenderError::Contract(format!(
                    "nitro proof for game {} carries undecodable transition public values: {error}",
                    game.address
                ))
            })?;
            Ok((
                domain_hash,
                parent_ref,
                l1_origin_number,
                transition,
                signature.clone(),
                public_key.clone(),
            )
                .abi_encode_params()
                .into())
        }
    }
}

#[cfg(test)]
mod encode_proof_tests {
    use super::*;
    use alloy_primitives::{Address, B256, address, b256};
    use alloy_sol_types::SolValue;

    fn metadata() -> GameMetadata {
        GameMetadata {
            address: address!("0000000000000000000000000000000000000001"),
            root_claim: b256!("2222222222222222222222222222222222222222222222222222222222222222"),
            l2_block_number: 100,
            l1_origin_hash: B256::repeat_byte(0x42),
            challenge_deadline: u64::MAX,
            proof_deadline: u64::MAX,
            proof_threshold: 2,
            domain_hash: B256::repeat_byte(0xd0),
            parent_ref: address!("0000000000000000000000000000000000001006"),
            l1_origin_number: 999,
        }
    }

    fn transition() -> TransitionPublicValues {
        TransitionPublicValues {
            l1Head: B256::repeat_byte(0x42),
            l2PreRoot: B256::repeat_byte(0x11),
            l2PreBlockNumber: 90,
            l2PostRoot: B256::repeat_byte(0x22),
            l2PostBlockNumber: 100,
            rollupConfigHash: B256::repeat_byte(0x33),
        }
    }

    /// Pins the payload `SP1ValidityVerifier._decodeAndBind` ABI-decodes. A bare concatenation of
    /// the prover's outputs — which this used to emit — fails to decode and is rejected onchain.
    #[test]
    fn sp1_payload_matches_verifier_tuple() {
        let game = metadata();
        let proof = ProofData::Sp1 {
            proof: Bytes::from_static(b"proof-bytes"),
            public_values: Bytes::from_static(b"public-values"),
        };

        let encoded = encode_proof(&game, &proof).expect("sp1 payload encodes");
        let (domain_hash, parent_ref, l1_origin_number, public_values, proof_bytes) =
            <(B256, Address, U256, Bytes, Bytes)>::abi_decode_params(&encoded)
                .expect("payload decodes as the verifier tuple");

        assert_eq!(domain_hash, game.domain_hash);
        assert_eq!(parent_ref, game.parent_ref);
        assert_eq!(l1_origin_number, U256::from(game.l1_origin_number));
        assert_eq!(public_values, Bytes::from_static(b"public-values"));
        assert_eq!(proof_bytes, Bytes::from_static(b"proof-bytes"));
    }

    /// Pins the payload `NitroProofVerifier._decodeAndBind` ABI-decodes. The transition is a
    /// static struct encoded inline, not an appended blob.
    #[test]
    fn nitro_payload_matches_verifier_tuple() {
        let game = metadata();
        let proof = ProofData::Nitro {
            attestation: Bytes::from_static(b"attestation"),
            public_values: transition().abi_encode().into(),
            signature: Bytes::from_static(b"signature"),
            public_key: Bytes::from_static(b"public-key"),
        };

        let encoded = encode_proof(&game, &proof).expect("nitro payload encodes");
        let (domain_hash, parent_ref, l1_origin_number, decoded, signature, public_key) =
            <(B256, Address, U256, TransitionPublicValues, Bytes, Bytes)>::abi_decode_params(
                &encoded,
            )
            .expect("payload decodes as the verifier tuple");

        assert_eq!(domain_hash, game.domain_hash);
        assert_eq!(parent_ref, game.parent_ref);
        assert_eq!(l1_origin_number, U256::from(game.l1_origin_number));
        assert_eq!(decoded, transition());
        assert_eq!(signature, Bytes::from_static(b"signature"));
        assert_eq!(public_key, Bytes::from_static(b"public-key"));
    }

    /// A transition the verifier could never decode must fail loudly at encode time rather than
    /// burning a lane submission on a payload that is guaranteed to be rejected.
    #[test]
    fn nitro_payload_rejects_undecodable_public_values() {
        let proof = ProofData::Nitro {
            attestation: Bytes::new(),
            public_values: Bytes::from_static(b"not a transition"),
            signature: Bytes::new(),
            public_key: Bytes::new(),
        };

        assert!(encode_proof(&metadata(), &proof).is_err());
    }
}
