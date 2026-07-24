use alloy_primitives::B256;
use tracing::{info, warn};
use world_chain_proofs::{ConsensusProvider, InvalidationReason, RootState};

use crate::{
    ParentRef, Proposal, ProposerClient, ProposerConfig, ProposerError,
    types::{CanonicalLine, CanonicalScan, FinalizedGames, NextProposalAction},
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
    pub fn new(config: ProposerConfig, execution_provider: E, consensus_provider: C) -> Self {
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
    /// Fetches the anchor, reconstructs its canonical descendants, and determines the next action.
    ///
    /// A game is considered canonical if it's built on top of a valid game and its root_claim
    /// is correct - i.e. it matches the one computed by the proposer itself.
    pub async fn anchor_and_canonical_line(&self) -> Result<CanonicalScan, ProposerError> {
        self.config.validate()?;

        let anchor_parent = self.execution_provider.anchor_parent().await?;
        let mut canonical_line = CanonicalLine::new(anchor_parent);

        let mut cursor = anchor_parent;
        let latest_finalized_l2_block = self.consensus_provider.latest_l2_finalized_block().await?;

        // loop to the next canonical game until it reaches the last one
        loop {
            let next_l2_block_number = cursor
                .l2_block_number
                .checked_add(self.config.block_interval)
                .ok_or(ProposerError::BlockNumberOverflow {
                    parent_block: cursor.l2_block_number,
                    block_interval: self.config.block_interval,
                })?;

            if next_l2_block_number > latest_finalized_l2_block {
                return Ok(CanonicalScan::new(
                    canonical_line,
                    NextProposalAction::CaughtUp {
                        target_block: next_l2_block_number,
                        finalized_block: latest_finalized_l2_block,
                    },
                ));
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
                // game already exists onchain, look at the resolution status now:
                // - if the root_state becomes `Invalidated`, then immediately return
                //   because we don't want to keep building on top of an invalid game
                // - otherwise, add the game to the canonical line and continue the loop
                let resolution_status = self
                    .execution_provider
                    .resolution_status(next_game_addr)
                    .await?;
                if resolution_status.root_state == RootState::Invalidated {
                    let next_action = if resolution_status.resolvable {
                        NextProposalAction::AwaitNegativeResolution {
                            game: next_game_addr,
                            reason: resolution_status.invalidation_reason,
                        }
                    } else if resolution_status.invalidation_reason
                        == InvalidationReason::ProofTimeout
                    {
                        NextProposalAction::RetryTimedOut {
                            proposal,
                            invalidated_game: next_game_addr,
                        }
                    } else {
                        NextProposalAction::BlockedByInvalidation {
                            game: next_game_addr,
                            reason: resolution_status.invalidation_reason,
                        }
                    };
                    return Ok(CanonicalScan::new(canonical_line, next_action));
                }

                let next_game = ParentRef {
                    address: next_game_addr,
                    l2_block_number: next_l2_block_number,
                };
                canonical_line.push_game(next_game);
                cursor = next_game;
            } else {
                // game doesn't exist onchain yet, exit the loop with a
                // `NextProposalAction::Propose`. The `propose` fn will
                // later publish this proposal onchain
                return Ok(CanonicalScan::new(
                    canonical_line,
                    NextProposalAction::Propose(proposal),
                ));
            }
        }
    }

    /// Resolves positively resolvable games parent-first and returns all finalized games.
    ///
    /// Games finalized by an earlier iteration or another keeper are included so anchor
    /// advancement can be retried.
    pub async fn resolve_games(
        &self,
        canonical_line: &CanonicalLine,
    ) -> Result<FinalizedGames, ProposerError> {
        let mut finalized_games = FinalizedGames::default();
        let mut resolutions_submitted = 0;
        for game in canonical_line.games() {
            let resolution_status = self
                .execution_provider
                .resolution_status(game.address)
                .await?;
            if resolution_status.positive_resolvable() {
                if resolutions_submitted >= self.config.max_resolutions_per_tick {
                    info!(
                        game_address = %game.address,
                        l2_block_number = game.l2_block_number,
                        max_resolutions_per_tick = self.config.max_resolutions_per_tick,
                        "skipping game resolution because proposer tick budget is exhausted"
                    );
                    continue;
                }
                // resolve the game
                let resolve_submission = self.execution_provider.resolve_game(game.address).await?;
                info!(
                    game_address = %game.address,
                    l2_block_number = game.l2_block_number,
                    tx_hash = ?resolve_submission.tx_hash,
                    "resolved World Chain proof-system game"
                );
                finalized_games.push(*game);
                resolutions_submitted += 1;
            } else if resolution_status.root_state == RootState::Finalized {
                // the game was finalized in an earlier iteration or by another keeper
                finalized_games.push(*game);
            }
        }
        Ok(finalized_games)
    }

    /// Advances the anchor to the highest finalized game, if one is available.
    pub async fn advance_anchor(
        &self,
        finalized_games: FinalizedGames,
    ) -> Result<(), ProposerError> {
        // games are ordered by l2 block number, therefore taking the last one
        // means taking the one with the highest l2 block number
        let maybe_highest_finalized_game = finalized_games.last();
        if let Some(highest_finalized_game) = maybe_highest_finalized_game {
            let close_game_submission = self
                .execution_provider
                .close_game(highest_finalized_game.address)
                .await?;
            info!(
                game_address = %highest_finalized_game.address,
                l2_block_number = highest_finalized_game.l2_block_number,
                tx_hash = ?close_game_submission.tx_hash,
                "closed World Chain proof-system game"
            );
        }
        Ok(())
    }

    /// Executes the next proposal action selected during canonical-line scanning.
    ///
    /// New and timed-out transitions are submitted, negative-ready games wait for the
    /// challenger, and non-retryable invalidations stop with a governance warning.
    ///
    pub async fn propose(&self, scan: &CanonicalScan) -> Result<(), ProposerError> {
        let (proposal, retry_of) = match scan.next_action() {
            NextProposalAction::Propose(proposal) => (proposal, None),
            NextProposalAction::RetryTimedOut {
                proposal,
                invalidated_game,
            } => (proposal, Some(*invalidated_game)),
            NextProposalAction::AwaitNegativeResolution { game, reason } => {
                warn!(
                    game_address = %game,
                    invalidation_reason = ?reason,
                    "waiting for challenger to resolve game with negative outcome"
                );
                return Ok(());
            }
            NextProposalAction::BlockedByInvalidation { game, reason } => {
                warn!(
                    game_address = %game,
                    invalidation_reason = ?reason,
                    "invalidated transition is not automatically retryable; governance intervention required"
                );
                return Ok(());
            }
            NextProposalAction::CaughtUp { .. } => return Ok(()),
        };

        let submission = self
            .execution_provider
            .submit_proposal(proposal, self.config.proposer_bond)
            .await?;
        info!(
            tx_hash = ?submission.tx_hash,
            game_address = %submission.game_address,
            l2_block_number = proposal.l2_block_number,
            parent_ref = %proposal.parent_ref,
            proposal_key = ?proposal.proposal_key,
            retry_of = ?retry_of,
            "submitted World Chain proof-system game"
        );
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
                let canonical_scan = self.anchor_and_canonical_line().await?;
                // 2. resolve positive-ready games parent-first
                let finalized_games = self.resolve_games(canonical_scan.canonical_line()).await?;
                // 3. advance the anchor to the highest finalized canonical game
                self.advance_anchor(finalized_games).await?;
                // 4. attempt a new canonical proposal or retry
                self.propose(&canonical_scan).await?;
                Ok(())
            }
            .await;

            if let Err(error) = iteration {
                warn!(%error, "proposer iteration failed; retrying on next tick");
            }
        }
    }
}
