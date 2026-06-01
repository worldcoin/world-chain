use alloy_primitives::{Address, B256, U256};
use async_trait::async_trait;
use world_chain_proofs::ProposalCommitment;

use crate::{ParentRef, Proposal, ProposalSubmission, ProposerError};

/// Minimal contract surface needed by the proposer.
#[async_trait]
pub trait ProofSystemClient: Send + Sync {
    /// Reads the parent state from the anchor registry.
    async fn anchor_parent(&self) -> Result<ParentRef, ProposerError>;

    /// Computes the deterministic proposal key used by the factory lookup.
    async fn proposal_key(&self, commitment: ProposalCommitment) -> Result<B256, ProposerError>;

    /// Returns an existing game for `proposal_key`, if one exists.
    async fn game_for_proposal_key(
        &self,
        proposal_key: B256,
    ) -> Result<Option<Address>, ProposerError>;

    /// Submits a proposal transaction to the factory.
    async fn submit_proposal(
        &self,
        proposal: &Proposal,
        proposer_bond: U256,
    ) -> Result<ProposalSubmission, ProposerError>;
}

/// Source for OP Stack output roots.
#[async_trait]
pub trait OutputRootProvider: Send + Sync {
    /// Returns the output root for an L2 block number.
    async fn output_root_at_block(&self, l2_block_number: u64) -> Result<B256, ProposerError>;
}
