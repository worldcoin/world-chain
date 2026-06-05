use alloy_primitives::B256;
use tracing::{debug, info, warn};
use world_chain_proofs::ConsensusProvider;

use crate::{
    ParentRef, Proposal, ProposalSubmission, ProposerClient, ProposerConfig, ProposerError,
};

/// World Chain Proposer.
#[derive(Debug)]
pub struct WorldChainProposer<E, C> {
    config: ProposerConfig,
    execution_provider: E,
    consensus_provider: C,
}

impl<E, C> WorldChainProposer<E, C> {
    /// Creates a proposer from execution and consensus providers.
    pub const fn new(config: ProposerConfig, execution_provider: E, consensus_provider: C) -> Self {
        Self {
            config,
            execution_provider,
            consensus_provider,
        }
    }

    /// Returns the proposer configuration.
    #[must_use]
    pub const fn config(&self) -> &ProposerConfig {
        &self.config
    }
}

impl<E, C> WorldChainProposer<E, C>
where
    E: ProposerClient,
    C: ConsensusProvider,
{
    /// Finds the first missing proposal after the current anchor.
    pub async fn prepare_next_proposal(&self) -> Result<Proposal, ProposerError> {
        self.config.validate()?;

        let mut parent = self.execution_provider.anchor_parent().await?;

        loop {
            let l2_block_number = parent
                .l2_block_number
                .checked_add(self.config.block_interval)
                .ok_or(ProposerError::BlockNumberOverflow {
                    parent_block: parent.l2_block_number,
                    block_interval: self.config.block_interval,
                })?;

            let latest_finalized_l2_block =
                self.consensus_provider.latest_l2_finalized_block().await?;
            if l2_block_number > latest_finalized_l2_block {
                return Err(ProposerError::ProposalNotReady {
                    target_block: l2_block_number,
                    finalized_block: latest_finalized_l2_block,
                });
            }

            let root_claim = self
                .consensus_provider
                .output_root_at_block(l2_block_number)
                .await?;
            let mut proposal = Proposal {
                parent_ref: parent.address,
                root_claim,
                l2_block_number,
                intermediate_roots_hash: B256::ZERO,
                proposal_key: B256::ZERO,
            };
            proposal.proposal_key = self
                .execution_provider
                .proposal_key(proposal.commitment())
                .await?;

            if let Some(game) = self
                .execution_provider
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
            .execution_provider
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
                Err(ProposerError::ProposalNotReady {
                    target_block,
                    finalized_block: l2_finalized_block,
                }) => {
                    debug!(
                        target_block,
                        l2_finalized_block, "waiting for L2 to finalize the next proposal height"
                    );
                }
                Err(error) => {
                    warn!(%error, "proposal attempt failed");
                }
            }
        }
    }
}
