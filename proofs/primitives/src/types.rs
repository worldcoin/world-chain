use alloy_primitives::{Address, B256, BlockNumber, U256, keccak256};
use alloy_sol_types::SolValue;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Number of distinct lanes required by WIP-1006 for challenged finality.
pub const PROOF_THRESHOLD: u8 = 2;

/// Number of configured proof lanes.
pub const PROOF_LANE_COUNT: u8 = 3;

/// Version of the World Chain proof-domain encoding implemented here.
pub const PROOF_SYSTEM_VERSION: u64 = 1;

/// The `GameCreated` event.
#[derive(Debug, Clone, Copy)]
pub struct GameCreated {
    pub proposal_key: B256,
    pub root_it: B256,
    pub game: Address,
    pub proposer: Address,
    pub root_claim: B256,
    pub l2_block_number: BlockNumber,
    pub parent_ref: Address,
    pub l1_origin_hash: B256,
    pub l1_origin_number: BlockNumber,
}

/// A game root state.
#[derive(Debug, PartialEq, Eq)]
pub enum RootState {
    None,
    Proposed,
    Challenged,
    Finalized,
    Invalidated,
}

#[derive(Debug, Error)]
pub enum RootStateError {
    #[error("Invalid root state: {0}")]
    InvalieRootState(u8),
}

impl TryFrom<u8> for RootState {
    type Error = RootStateError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(RootState::None),
            1 => Ok(RootState::Proposed),
            2 => Ok(RootState::Challenged),
            3 => Ok(RootState::Finalized),
            4 => Ok(RootState::Invalidated),
            _ => Err(RootStateError::InvalieRootState(value)),
        }
    }
}

/// Domain constants committed into every root id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofDomain {
    /// World Chain L2 chain id.
    pub chain_id: u64,
    /// Proof-system encoding version.
    pub proof_system_version: u64,
    /// Hash of the rollup config and World Chain hardfork schedule.
    pub rollup_config_hash: B256,
    /// Distance in L2 blocks between parent and proposed roots.
    pub block_interval: u64,
    /// Distance in L2 blocks between intermediate roots.
    pub intermediate_block_interval: u64,
}

impl ProofDomain {
    /// Compute the Solidity-compatible domain hash.
    #[must_use]
    pub fn hash(self) -> B256 {
        let encoded = (
            U256::from(self.chain_id),
            U256::from(self.proof_system_version),
            self.rollup_config_hash,
            U256::from(self.block_interval),
            U256::from(self.intermediate_block_interval),
        )
            .abi_encode();
        keccak256(encoded)
    }
}

/// Per-proposal commitment fields used to compute a canonical root id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalCommitment {
    /// Parent `AnchorStateRegistry` or parent game address.
    pub parent_ref: Address,
    /// Claimed OP Stack output root.
    pub root_claim: B256,
    /// L2 block number for `root_claim`.
    pub l2_block_number: u64,
    /// Commitment to ordered intermediate roots, or zero if unused.
    pub intermediate_roots_hash: B256,
}

impl ProposalCommitment {
    /// Compute the Solidity-compatible proposal key used for factory lookups.
    #[must_use]
    pub fn proposal_key(self, domain_hash: B256) -> B256 {
        let encoded = (
            domain_hash,
            self.parent_ref,
            self.root_claim,
            U256::from(self.l2_block_number),
            self.intermediate_roots_hash,
        )
            .abi_encode();
        keccak256(encoded)
    }
}

/// Per-proposal commitment fields used to compute a canonical root id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootCommitment {
    /// Proposal fields supplied by the proposer.
    pub proposal: ProposalCommitment,
    /// L1 origin hash pinned by the proposal factory.
    pub l1_origin_hash: B256,
    /// L1 origin block number paired with `l1_origin_hash`.
    pub l1_origin_number: u64,
}

impl RootCommitment {
    /// Compute the Solidity-compatible proposal key used for factory lookups.
    #[must_use]
    pub fn proposal_key(self, domain_hash: B256) -> B256 {
        self.proposal.proposal_key(domain_hash)
    }

    /// Compute the Solidity-compatible root id for this proposal.
    #[must_use]
    pub fn root_id(self, domain_hash: B256) -> B256 {
        let encoded = (
            domain_hash,
            self.proposal.parent_ref,
            self.proposal.root_claim,
            U256::from(self.proposal.l2_block_number),
            self.proposal.intermediate_roots_hash,
            self.l1_origin_hash,
            U256::from(self.l1_origin_number),
        )
            .abi_encode();
        keccak256(encoded)
    }
}

/// WIP-1006 proof lanes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum ProofLane {
    /// zkVM, SNARK, or equivalent validity proof.
    ValidityProof = 0,
    /// TEE signer attestation.
    TeeAttestation = 1,
    /// Security Council attestation.
    SecurityCouncil = 2,
}

impl ProofLane {
    /// Bit assigned to this lane in the per-root proof bitmap.
    #[must_use]
    pub const fn mask(self) -> u8 {
        1 << self as u8
    }
}

/// Count distinct lanes in a proof bitmap.
#[must_use]
pub const fn proof_count(bitmap: u8) -> u8 {
    let mut count = 0;
    let mut index = 0;
    while index < PROOF_LANE_COUNT {
        if bitmap & (1 << index) != 0 {
            count += 1;
        }
        index += 1;
    }
    count
}

/// Whether a bitmap satisfies the WIP-1006 threshold.
#[must_use]
pub const fn has_threshold(bitmap: u8) -> bool {
    proof_count(bitmap) >= PROOF_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, b256};

    #[test]
    fn lane_bitmap_counts_distinct_lanes() {
        let bitmap = ProofLane::ValidityProof.mask() | ProofLane::SecurityCouncil.mask();

        assert_eq!(proof_count(bitmap), 2);
        assert!(has_threshold(bitmap));
        assert!(!has_threshold(ProofLane::TeeAttestation.mask()));
    }

    #[test]
    fn root_id_changes_when_domain_changes() {
        let domain = ProofDomain {
            chain_id: 4801,
            proof_system_version: PROOF_SYSTEM_VERSION,
            rollup_config_hash: b256!(
                "1111111111111111111111111111111111111111111111111111111111111111"
            ),
            block_interval: 10,
            intermediate_block_interval: 5,
        };
        let proposal = ProposalCommitment {
            parent_ref: address!("0000000000000000000000000000000000001006"),
            root_claim: b256!("2222222222222222222222222222222222222222222222222222222222222222"),
            l2_block_number: 10,
            intermediate_roots_hash: B256::ZERO,
        };
        let commitment = RootCommitment {
            proposal,
            l1_origin_hash: b256!(
                "3333333333333333333333333333333333333333333333333333333333333333"
            ),
            l1_origin_number: 1,
        };

        let root_id = commitment.root_id(domain.hash());
        assert_ne!(B256::ZERO, proposal.proposal_key(domain.hash()));
        let changed = ProofDomain {
            chain_id: 4802,
            ..domain
        };

        assert_ne!(root_id, commitment.root_id(changed.hash()));
    }
}
