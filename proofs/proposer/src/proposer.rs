use alloy_primitives::B256;
use tracing::{info, warn};

use crate::{
    OutputRootProvider, ParentRef, ProofSystemClient, Proposal, ProposalSubmission, ProposerConfig,
    ProposerError,
};

/// World Chain Proposer.
#[derive(Debug)]
pub struct WorldChainProposer<C, O> {
    config: ProposerConfig,
    contracts: C,
    output_roots: O,
}

impl<C, O> WorldChainProposer<C, O> {
    /// Creates a proposer from contract and output-root clients.
    pub const fn new(config: ProposerConfig, contracts: C, output_roots: O) -> Self {
        Self {
            config,
            contracts,
            output_roots,
        }
    }

    /// Returns the proposer configuration.
    #[must_use]
    pub const fn config(&self) -> &ProposerConfig {
        &self.config
    }
}

impl<C, O> WorldChainProposer<C, O>
where
    C: ProofSystemClient,
    O: OutputRootProvider,
{
    /// Finds the first missing proposal after the current anchor.
    pub async fn prepare_next_proposal(&self) -> Result<Proposal, ProposerError> {
        self.config.validate()?;

        let mut parent = self.contracts.anchor_parent().await?;

        loop {
            let l2_block_number = parent
                .l2_block_number
                .checked_add(self.config.block_interval)
                .ok_or(ProposerError::BlockNumberOverflow {
                    parent_block: parent.l2_block_number,
                    block_interval: self.config.block_interval,
                })?;

            let root_claim = self
                .output_roots
                .output_root_at_block(l2_block_number)
                .await?;
            let mut proposal = Proposal {
                parent_ref: parent.address,
                root_claim,
                l2_block_number,
                intermediate_roots_hash: B256::ZERO,
                proposal_key: B256::ZERO,
            };
            proposal.proposal_key = self.contracts.proposal_key(proposal.commitment()).await?;

            if let Some(game) = self
                .contracts
                .game_for_proposal_key(proposal.proposal_key)
                .await?
            {
                parent = ParentRef {
                    address: game,
                    l2_block_number,
                };
                continue;
            }

            return Ok(proposal);
        }
    }

    /// Prepares and submits one proposal, if the next expected game is missing.
    pub async fn propose_once(&self) -> Result<(Proposal, ProposalSubmission), ProposerError> {
        let proposal = self.prepare_next_proposal().await?;
        let submission = self
            .contracts
            .submit_proposal(&proposal, self.config.proposer_bond)
            .await?;
        Ok((proposal, submission))
    }

    /// Runs the proposer forever, logging transient failures and retrying on each tick.
    pub async fn run_forever(&self) -> Result<(), ProposerError> {
        self.config.validate()?;

        let mut interval = tokio::time::interval(self.config.poll_interval);
        loop {
            interval.tick().await;
            match self.propose_once().await {
                Ok((proposal, submission)) => {
                    info!(
                        l2_block_number = proposal.l2_block_number,
                        parent_ref = %proposal.parent_ref,
                        proposal_key = ?proposal.proposal_key,
                        tx_hash = ?submission.tx_hash,
                        "submitted World Chain proof-system game"
                    );
                }
                Err(error) => {
                    warn!(%error, "proposal attempt failed");
                }
            }
        }
    }
}
