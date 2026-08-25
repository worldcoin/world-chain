use tracing::{info, warn};
use world_chain_proof_protocol::{
    ConsensusProvider, GameStatus, InvalidationReason, LineageStop, SelectedLineageGame,
    select_lineage,
};

use crate::{
    Proposal, ProposerClient, ProposerConfig, ProposerError,
    types::{NextProposalAction, ProposerScan},
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
    /// Fetches the anchor, reconstructs its canonical descendants, and determines the next action.
    ///
    /// A game is considered canonical if it's built on top of a valid game and its root_claim
    /// is correct - i.e. it matches the one computed by the proposer itself.
    pub async fn scan_selected_lineage(&self) -> Result<ProposerScan, ProposerError> {
        self.config.validate()?;

        let lineage = select_lineage(&self.execution_provider, &self.consensus_provider).await?;
        let selected_l2_block_number = lineage
            .games()
            .last()
            .map_or(lineage.anchor().l2_block_number, |selected| {
                selected.transition.l2_block_number
            });
        crate::metrics::set_selected_lineage_l2_block_number(selected_l2_block_number);
        let next_action = match lineage.stop() {
            LineageStop::CaughtUp {
                target_block,
                finalized_block,
            } => NextProposalAction::CaughtUp {
                target_block,
                finalized_block,
            },
            LineageStop::Missing(transition) => NextProposalAction::Propose(Proposal {
                parent_ref: transition.parent_ref,
                root_claim: transition.root_claim,
                l2_block_number: transition.l2_block_number,
                attempt: 0,
            }),
            LineageStop::Invalidated {
                transition,
                game,
                status,
            } => {
                if status.resolvable {
                    NextProposalAction::ResolveNegative {
                        game: game.address,
                        reason: status.invalidation_reason,
                    }
                } else if status.invalidation_reason == InvalidationReason::ProofTimeout {
                    // A retry must reuse the invalidated game's transition commitment.
                    NextProposalAction::RetryTimedOut {
                        proposal: Proposal {
                            parent_ref: transition.parent_ref,
                            root_claim: transition.root_claim,
                            l2_block_number: transition.l2_block_number,
                            attempt: game.attempt.checked_add(1).ok_or(ProposerError::Overflow)?,
                        },
                        invalidated_game: game.address,
                    }
                } else {
                    NextProposalAction::BlockedByInvalidation {
                        game: game.address,
                        reason: status.invalidation_reason,
                    }
                }
            }
        };

        Ok(ProposerScan::new(lineage, next_action))
    }

    /// Resolves positively resolvable games parent-first and returns all defender-winning games.
    ///
    /// Games resolved by an earlier iteration or another keeper are included so anchor
    /// advancement can be retried.
    pub async fn resolve_games(
        &self,
        games: &[SelectedLineageGame],
    ) -> Result<Vec<SelectedLineageGame>, ProposerError> {
        let mut resolved_games = Vec::new();
        let mut resolutions_submitted = 0;
        for selected in games {
            let game = selected.game;
            let resolution_status = self
                .execution_provider
                .lineage_resolution_status(game.address)
                .await?;
            if resolution_status.positive_resolvable() {
                if resolutions_submitted >= self.config.max_resolutions_per_tick {
                    info!(
                        game_address = %game.address,
                        l2_block_number = selected.transition.l2_block_number,
                        max_resolutions_per_tick = self.config.max_resolutions_per_tick,
                        "skipping game resolution because proposer tick budget is exhausted"
                    );
                    continue;
                }
                let resolve_submission = self.execution_provider.resolve_game(game.address).await?;
                info!(
                    lifecycle_event = "game_resolved",
                    outcome = "positive",
                    game_address = %game.address,
                    l2_block_number = selected.transition.l2_block_number,
                    tx_hash = ?resolve_submission.tx_hash,
                    "resolved World Chain proof-system game"
                );
                resolved_games.push(*selected);
                resolutions_submitted += 1;
            } else if resolution_status.outcome == GameStatus::DefenderWins {
                // the game was resolved in an earlier iteration or by another keeper
                resolved_games.push(*selected);
            }
        }
        Ok(resolved_games)
    }

    /// Advances the anchor to the highest ASR-valid defender-winning game, if one is available.
    pub async fn advance_anchor(
        &self,
        resolved_games: &[SelectedLineageGame],
    ) -> Result<(), ProposerError> {
        for selected in resolved_games.iter().rev() {
            if !self
                .execution_provider
                .is_game_claim_valid(selected.game.address)
                .await?
            {
                continue;
            }

            let close_game_submission = self
                .execution_provider
                .close_game(selected.game.address)
                .await?;
            info!(
                game_address = %selected.game.address,
                l2_block_number = selected.transition.l2_block_number,
                tx_hash = ?close_game_submission.tx_hash,
                "closed World Chain proof-system game"
            );
            break;
        }
        Ok(())
    }

    /// Resolves a negative tip or creates the next selected game or retry.
    pub async fn submit_next_proposal(&self, scan: &ProposerScan) -> Result<(), ProposerError> {
        let (proposal, retry_of) = match scan.next_action() {
            NextProposalAction::Propose(proposal) => (*proposal, None),
            NextProposalAction::RetryTimedOut {
                proposal,
                invalidated_game,
            } => {
                warn!(
                    invalidated_game = %invalidated_game,
                    parent_ref = %proposal.parent_ref,
                    root_claim = %proposal.root_claim,
                    l2_block_number = proposal.l2_block_number,
                    attempt = proposal.attempt,
                    "creating a retry; bond manager will recover proposer-owned descendants invalidated by the old attempt"
                );
                (*proposal, Some(*invalidated_game))
            }
            NextProposalAction::ResolveNegative { game, reason } => {
                let submission = self.execution_provider.resolve_game(*game).await?;
                info!(
                    lifecycle_event = "game_resolved",
                    outcome = "negative",
                    game_address = %game,
                    invalidation_reason = ?reason,
                    tx_hash = ?submission.tx_hash,
                    "resolved selected game with its negative outcome"
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

        let submission = self.execution_provider.submit_proposal(&proposal).await?;
        crate::metrics::set_selected_lineage_l2_block_number(proposal.l2_block_number);
        crate::metrics::increment_proposals_submitted(if retry_of.is_some() {
            "retry"
        } else {
            "new"
        });
        info!(
            lifecycle_event = "proposal_submitted",
            proposal_kind = if retry_of.is_some() { "retry" } else { "new" },
            tx_hash = ?submission.tx_hash,
            game_address = %submission.game_address,
            l2_block_number = proposal.l2_block_number,
            parent_ref = %proposal.parent_ref,
            attempt = proposal.attempt,
            retry_of = ?retry_of,
            "submitted World Chain proof-system game"
        );
        Ok(())
    }

    pub async fn tick(&self) -> Result<(), ProposerError> {
        // 1. refresh the anchor and canonical line
        let scan = self.scan_selected_lineage().await?;
        // 2. resolve positive-ready games parent-first
        let resolved_games = self.resolve_games(scan.lineage().games()).await?;
        // 3. advance the anchor to the highest ASR-valid resolved game
        self.advance_anchor(&resolved_games).await?;
        // 4. resolve a negative tip, or submit a new proposal or retry
        self.submit_next_proposal(&scan).await?;
        Ok(())
    }

    /// Runs the proposer forever, logging transient failures and retrying on each tick.
    pub async fn run_forever(&self) -> Result<(), ProposerError> {
        self.config.validate()?;

        let mut interval = tokio::time::interval(self.config.poll_interval);
        loop {
            interval.tick().await;
            if let Err(error) = self.tick().await {
                warn!(%error, "proposer iteration failed; retrying on next tick");
            }
        }
    }
}
