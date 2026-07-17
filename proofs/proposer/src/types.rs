use alloy_primitives::{Address, B256, TxHash};
use world_chain_proofs::ProposalCommitment;

/// A parent reference that the next proposal should build on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParentRef {
    /// Address of the anchor registry or parent game.
    pub address: Address,
    /// L2 block number of the parent output root.
    pub l2_block_number: u64,
}

/// Candidate proposal data supplied to the proof-system factory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proposal {
    /// Address of the anchor registry or parent game.
    pub parent_ref: Address,
    /// Claimed OP Stack output root.
    pub root_claim: B256,
    /// L2 block number for `root_claim`.
    pub l2_block_number: u64,
    /// Deterministic factory lookup key, excluding L1 origin.
    pub proposal_key: B256,
}

impl Proposal {
    /// Returns the proposal commitment used to compute the factory lookup key.
    #[must_use]
    pub const fn commitment(&self) -> ProposalCommitment {
        ProposalCommitment {
            parent_ref: self.parent_ref,
            root_claim: self.root_claim,
            l2_block_number: self.l2_block_number,
        }
    }
}

/// Result of a submitted proposal transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProposalSubmission {
    /// Transaction hash for the proposal submission.
    pub tx_hash: TxHash,
}
