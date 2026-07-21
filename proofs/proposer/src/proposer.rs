use alloy_primitives::B256;
use tracing::{debug, info, warn};
use world_chain_proofs::ConsensusProvider;

use crate::{
    ParentRef, Proposal, ProposalSubmission, ProposerClient, ProposerConfig, ProposerError,
    types::{CanonicalLine, ResolvedGames},
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
    /// Fetch the anchor game and reconconstruct the canonical line of games that is built on
    /// top of the current anchor (if any) until the last created canonical game.
    ///
    ///
    /// A game is considered canonical if it's built on top of a valid game and its root_claim
    /// is correct - i.e. it matches the one computed by the proposer itself.
    pub async fn anchor_and_canonical_line(&self) -> Result<CanonicalLine, ProposerError> {
        self.config.validate()?;

        let mut canonical_line = CanonicalLine::default();

        let anchor_game = self.execution_provider.anchor_parent().await?;
        canonical_line.push(anchor_game);

        let mut cursor = anchor_game;

        // loop to the next canonical game until it reaches the last one
        loop {
            let next_l2_block_number = cursor
                .l2_block_number
                .checked_add(self.config.block_interval)
                .ok_or(ProposerError::BlockNumberOverflow {
                    parent_block: cursor.l2_block_number,
                    block_interval: self.config.block_interval,
                })?;

            let latest_finalized_l2_block =
                self.consensus_provider.latest_l2_finalized_block().await?;
            if next_l2_block_number > latest_finalized_l2_block {
                return Err(ProposerError::ProposalNotReady {
                    target_block: next_l2_block_number,
                    finalized_block: latest_finalized_l2_block,
                });
            }

            let root_claim = self
                .consensus_provider
                .output_root_at_block(next_l2_block_number)
                .await?;
            let mut proposal = Proposal {
                parent_ref: cursor.address,
                root_claim,
                l2_block_number: next_l2_block_number,
                proposal_key: B256::ZERO,
            };
            proposal.proposal_key = self
                .execution_provider
                .proposal_key(proposal.commitment())
                .await?;

            if let Some(next_game_addr) = self
                .execution_provider
                .game_for_proposal_key(proposal.proposal_key)
                .await?
            {
                let next_game = ParentRef {
                    address: next_game_addr,
                    l2_block_number: next_l2_block_number,
                };
                canonical_line.push(next_game);
                cursor = next_game;
            } else {
                break;
            }
        }
        Ok(canonical_line)
    }

    pub async fn resolve_games(
        &self,
        canonical_line: &CanonicalLine,
    ) -> Result<ResolvedGames, ProposerError> {
        let mut resolved_games = ResolvedGames::default();
        for game in canonical_line {
            let resolution_status = self
                .execution_provider
                .resolution_status(game.address)
                .await?;
            if resolution_status.positive_resolvable() {
                // resolve the game
                let resolve_submission = self.execution_provider.resolve_game(game.address).await?;
                info!(
                    game_address = %game.address,
                    l2_block_number = game.l2_block_number,
                    tx_hash = ?resolve_submission.tx_hash,
                    "resolved World Chain proof-system game"
                );
                // add resolved game to the result list
                resolved_games.push(game);
            }
        }
        Ok(resolved_games)
    }

    pub async fn advance_anchor(&self, resolved_games: ResolvedGames) -> Result<(), ProposerError> {
        // games are ordered by l2 block number, therefore taking the last one
        // means taking the one with the highest l2 block number
        let maybe_highest_resolved_game = resolved_games.last();
        if let Some(highest_resolved_game) = maybe_highest_resolved_game {
            let close_game_submission = self
                .execution_provider
                .close_game(highest_resolved_game.address)
                .await?;
            info!(
                game_address = %highest_resolved_game.address,
                l2_block_number = highest_resolved_game.l2_block_number,
                tx_hash = ?close_game_submission.tx_hash,
                "closed World Chain proof-system game"
            );
        }
        Ok(())
    }

    pub async fn propose(&self, canonical_line: &CanonicalLine) -> Result<(), ProposerError> {
        let maybe_last_canonical_game = canonical_line.last();
        if let Some(last_canonical_game) = maybe_last_canonical_game {
            let mut cursor = last_canonical_game;
            let proposal = loop {
                let next_l2_block_number = cursor
                    .l2_block_number
                    .checked_add(self.config.block_interval)
                    .ok_or(ProposerError::BlockNumberOverflow {
                        parent_block: cursor.l2_block_number,
                        block_interval: self.config.block_interval,
                    })?;

                let latest_finalized_l2_block =
                    self.consensus_provider.latest_l2_finalized_block().await?;
                if next_l2_block_number > latest_finalized_l2_block {
                    return Err(ProposerError::ProposalNotReady {
                        target_block: next_l2_block_number,
                        finalized_block: latest_finalized_l2_block,
                    });
                }
                let root_claim = self
                    .consensus_provider
                    .output_root_at_block(next_l2_block_number)
                    .await?;
                let mut proposal = Proposal {
                    parent_ref: cursor.address,
                    root_claim,
                    l2_block_number: next_l2_block_number,
                    proposal_key: B256::ZERO,
                };
                proposal.proposal_key = self
                    .execution_provider
                    .proposal_key(proposal.commitment())
                    .await?;
                if let Some(next_game_addr) = self
                    .execution_provider
                    .game_for_proposal_key(proposal.proposal_key)
                    .await?
                {
                    // game already exists, build on top of it
                    cursor = ParentRef {
                        address: next_game_addr,
                        l2_block_number: next_l2_block_number,
                    };
                } else {
                    break proposal;
                }
            };
            let submission = self
                .execution_provider
                .submit_proposal(&proposal, self.config.proposer_bond)
                .await?;
            info!(
                tx_hash = ?submission.tx_hash,
                l2_block_number = proposal.l2_block_number,
                parent_ref = %proposal.parent_ref,
                proposal_key = ?proposal.proposal_key,
                "submitted World Chain proof-system game"
            );
        }
        Ok(())
    }

    /// Runs the proposer forever, logging transient failures and retrying on each tick.
    pub async fn run_forever(&self) -> Result<(), ProposerError> {
        self.config.validate()?;

        let mut interval = tokio::time::interval(self.config.poll_interval);
        loop {
            interval.tick().await;
            let iteration: Result<(), ProposerError> = async {
                // 1. refresh the anchor and canonical line
                let canonical_line = self.anchor_and_canonical_line().await?;
                // 2. resolve positive-ready games parent-first
                let resolved_games = self.resolve_games(&canonical_line).await?;
                // 3. advance the anchor to the highest finalized canonical game
                self.advance_anchor(resolved_games).await?;
                // 4. attempt a new canonical proposal or retry
                self.propose(&canonical_line).await?;
                // 5. withdraw known proposer credits
                // TODO: add withdraw credits logic
                Ok(())
            }
            .await;

            if let Err(error) = iteration {
                warn!(%error, "proposer iteration failed; retrying on next tick");
            }
        }
    }
}
