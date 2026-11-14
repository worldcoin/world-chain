//! Public verification of the original Flashblocks P2P frame, without JSON re-encoding.

use alloy_primitives::{B256, Bytes};
use alloy_rpc_types_engine::PayloadId;
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    error::FlashblocksError,
    p2p::{Authorized, AuthorizedMsg},
    primitives::FlashblocksPayloadV1,
};

/// The existing P2P frame limit, also bounding evidence decoding and RPC responses.
pub const MAX_FLASHBLOCK_EVIDENCE_BYTES: usize = 1 << 24;
/// Versioned capability for the read-only evidence lookup.
pub const FLASHBLOCK_EVIDENCE_CAPABILITY: &str = "worldchain_flashblock_evidence_v1";

/// An immutable signed view; payload ID and index alone are not a block commitment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlashblockEvidenceId {
    pub payload_id: PayloadId,
    pub index: u64,
    pub block_hash: B256,
}

impl From<&FlashblocksPayloadV1> for FlashblockEvidenceId {
    fn from(payload: &FlashblocksPayloadV1) -> Self {
        Self {
            payload_id: payload.payload_id,
            index: payload.index,
            block_hash: payload.diff.block_hash,
        }
    }
}

/// Original signature-verified wire evidence, independent of execution/finality status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlashblockEvidence {
    pub version: u32,
    /// RPC context only: the existing Ed25519 preimage does not contain a chain ID.
    #[serde(with = "alloy_serde::quantity")]
    pub chain_id: u64,
    pub id: FlashblockEvidenceId,
    /// Complete uncompressed flblk frame, including its message-type byte.
    pub wire: Bytes,
}

#[derive(Debug, Error)]
pub enum FlashblockEvidenceError {
    #[error("unsupported evidence version or chain context")]
    Context,
    #[error("evidence does not match the requested immutable view")]
    View,
    #[error("invalid evidence frame length or message type")]
    Frame,
    #[error(transparent)]
    Signature(#[from] FlashblocksError),
}

impl FlashblockEvidence {
    /// Verify with a separately trusted network-specific authorizer and requested view.
    /// Chain context is checked locally; cross-network isolation requires distinct trusted keys.
    pub fn verify(
        &self,
        expected_chain_id: u64,
        authorizer: VerifyingKey,
        expected: FlashblockEvidenceId,
    ) -> Result<FlashblocksPayloadV1, FlashblockEvidenceError> {
        if self.version != 1 || self.chain_id != expected_chain_id {
            return Err(FlashblockEvidenceError::Context);
        }
        if self.id != expected {
            return Err(FlashblockEvidenceError::View);
        }
        if self.wire.len() > MAX_FLASHBLOCK_EVIDENCE_BYTES || self.wire.first() != Some(&0) {
            return Err(FlashblockEvidenceError::Frame);
        }
        let mut remaining = &self.wire[1..];
        let parts = Authorized::parse(&mut remaining)?;
        if !remaining.is_empty() {
            return Err(FlashblockEvidenceError::Frame);
        }
        parts.verify(authorizer)?;
        let authorized = parts.into_authorized()?;
        let AuthorizedMsg::FlashblocksPayloadV1(payload) = authorized.msg else {
            return Err(FlashblockEvidenceError::Frame);
        };
        if FlashblockEvidenceId::from(&payload) != expected
            || authorized.authorization.payload_id != payload.payload_id
        {
            return Err(FlashblockEvidenceError::View);
        }
        Ok(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::p2p::FlashblocksP2PMsg;
    use ed25519_dalek::SigningKey;

    fn fixture() -> (FlashblockEvidence, VerifyingKey) {
        #[derive(Deserialize)]
        struct Fixture {
            authorizer: B256,
            evidence: FlashblockEvidence,
        }
        let fixture: Fixture =
            serde_json::from_str(include_str!("../testdata/flashblock-evidence-v1.json"))
                .expect("public verification vector");
        (
            fixture.evidence,
            VerifyingKey::from_bytes(&fixture.authorizer.0).expect("fixture authorizer"),
        )
    }

    #[test]
    fn verifies_original_bytes_and_rejects_normalized_metadata() {
        let (evidence, authorizer) = fixture();
        evidence
            .verify(480, authorizer, evidence.id)
            .expect("original signatures verify");
        let mut bytes = evidence.wire.as_ref();
        let FlashblocksP2PMsg::Authorized(normalized) =
            FlashblocksP2PMsg::decode(&mut bytes).expect("decode fixture")
        else {
            panic!("wrong fixture type");
        };
        assert!(
            normalized.verify(authorizer).is_err(),
            "JSON re-encoding changes the signed preimage"
        );
        let json = serde_json::to_vec(&evidence).expect("serialize evidence");
        let decoded: FlashblockEvidence =
            serde_json::from_slice(&json).expect("deserialize evidence");
        assert_eq!(decoded, evidence);
        decoded
            .verify(480, authorizer, evidence.id)
            .expect("RPC JSON preserves wire bytes");
    }

    #[test]
    fn rejects_wrong_trust_context_view_and_tampering() {
        let (evidence, authorizer) = fixture();
        assert!(evidence.verify(481, authorizer, evidence.id).is_err());
        assert!(
            evidence
                .verify(
                    480,
                    SigningKey::from_bytes(&[8; 32]).verifying_key(),
                    evidence.id
                )
                .is_err()
        );
        let mut wrong = evidence.id;
        wrong.block_hash = B256::with_last_byte(1);
        assert!(evidence.verify(480, authorizer, wrong).is_err());
        let mut altered = evidence.clone();
        altered.id = wrong;
        assert!(altered.verify(480, authorizer, wrong).is_err());
        let mut wire = evidence.wire.to_vec();
        let last = wire.len() - 1;
        wire[last] ^= 1;
        altered = evidence.clone();
        altered.wire = wire.into();
        assert!(altered.verify(480, authorizer, evidence.id).is_err());
        let mut trailing = evidence.wire.to_vec();
        trailing.push(0);
        assert!(FlashblocksP2PMsg::parse(&trailing).is_err());
        altered.wire = trailing.into();
        assert!(altered.verify(480, authorizer, evidence.id).is_err());
        for wire in [
            vec![],
            vec![0],
            vec![1],
            vec![0; MAX_FLASHBLOCK_EVIDENCE_BYTES + 1],
        ] {
            altered.wire = wire.into();
            assert!(altered.verify(480, authorizer, evidence.id).is_err());
        }
    }
}
