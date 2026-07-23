use alloy_primitives::{Address, U256};
use tracing::{info, warn};
use world_chain_proofs::{
    ANCHOR_PARENT_INDEX, ConsensusProvider, InvalidationReason, ResolutionStatus, RootState,
};

use crate::{
    ParentRef, Proposal, ProposerClient, ProposerConfig, ProposerError,
    types::{CanonicalLine, CanonicalScan, FinalizedGames, NextProposalAction},
};

/// Defensive bound on the retry-attempt probe loop; every attempt on-chain cost a full
/// proposer bond, so runs anywhere near this length cannot occur in practice.
const MAX_ATTEMPT_PROBES: u64 = 256;

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

/// The highest-attempt game found for one transition under one parent candidate.
#[derive(Debug, Clone, Copy)]
struct TransitionGame {
    parent: ParentRef,
    game: Address,
    attempt: U256,
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
    ///
    /// Each transition is looked up under every valid parent candidate: a parent that is the
    /// current anchor is addressable both by its factory index (children created before the
    /// anchor advanced) and by the anchor sentinel (children created after), so both lineages
    /// are searched, along with every retry attempt.
    pub async fn anchor_and_canonical_line(&self) -> Result<CanonicalScan, ProposerError> {
        self.config.validate()?;

        let anchor_parents = self.execution_provider.anchor_parents().await?;
        let sentinel = anchor_parents
            .iter()
            .copied()
            .find(ParentRef::is_anchor)
            .ok_or(ProposerError::InvalidConfig(
                "anchor_parents must include the anchor sentinel",
            ))?;
        let anchor_l2_block = sentinel.l2_block_number;
        let mut canonical_line = CanonicalLine::new(sentinel);

        // Parent candidates for the current step; all entries share one L2 block number.
        let mut cursors = anchor_parents;
        let latest_finalized_l2_block = self.consensus_provider.latest_l2_finalized_block().await?;

        loop {
            let parent_l2_block = cursors[0].l2_block_number;
            let next_l2_block_number = parent_l2_block
                .checked_add(self.config.block_interval)
                .ok_or(ProposerError::BlockNumberOverflow {
                    parent_block: parent_l2_block,
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

            // A new proposal must reference the anchor sentinel when its parent is the current
            // anchor; the game contract rejects index-addressed parents at or below the anchor.
            let propose_parent = if parent_l2_block == anchor_l2_block {
                sentinel
            } else {
                cursors[0]
            };

            // Find the highest-attempt game per parent candidate.
            let mut found_games = Vec::with_capacity(cursors.len());
            for candidate in &cursors {
                if let Some(found) = self
                    .latest_attempt(*candidate, root_claim, next_l2_block_number)
                    .await?
                {
                    found_games.push(found);
                }
            }

            // Prefer a live-or-finalized game (canonical continuation) over invalidated ones.
            let mut invalidated: Option<(TransitionGame, ResolutionStatus)> = None;
            let mut canonical: Option<TransitionGame> = None;
            for found in &found_games {
                let resolution_status = self
                    .execution_provider
                    .resolution_status(found.game)
                    .await?;
                if resolution_status.root_state == RootState::Invalidated {
                    if invalidated.is_none() {
                        invalidated = Some((*found, resolution_status));
                    }
                } else {
                    canonical = Some(*found);
                    break;
                }
            }

            if let Some(found) = canonical {
                let parent_index = self.execution_provider.game_index(found.game).await?;
                let next_ref = ParentRef {
                    address: found.game,
                    l2_block_number: next_l2_block_number,
                    parent_index,
                };
                canonical_line.push_game(next_ref);
                cursors = vec![next_ref];
                continue;
            }

            let next_action = match invalidated {
                Some((found, resolution_status)) => self.invalidated_transition_action(
                    &found_games,
                    found,
                    resolution_status,
                    propose_parent,
                    root_claim,
                    next_l2_block_number,
                    anchor_l2_block,
                ),
                None => NextProposalAction::Propose(Proposal {
                    parent_index: propose_parent.parent_index,
                    parent_ref: propose_parent.address,
                    root_claim,
                    l2_block_number: next_l2_block_number,
                    attempt: U256::ZERO,
                }),
            };
            return Ok(CanonicalScan::new(canonical_line, next_action));
        }
    }

    /// Determines the action for a transition whose only existing games are invalidated.
    #[allow(clippy::too_many_arguments)]
    fn invalidated_transition_action(
        &self,
        found_games: &[TransitionGame],
        found: TransitionGame,
        resolution_status: ResolutionStatus,
        propose_parent: ParentRef,
        root_claim: alloy_primitives::B256,
        l2_block_number: u64,
        anchor_l2_block: u64,
    ) -> NextProposalAction {
        if resolution_status.resolvable {
            return NextProposalAction::AwaitNegativeResolution {
                game: found.game,
                reason: resolution_status.invalidation_reason,
            };
        }
        if resolution_status.invalidation_reason != InvalidationReason::ProofTimeout {
            return NextProposalAction::BlockedByInvalidation {
                game: found.game,
                reason: resolution_status.invalidation_reason,
            };
        }

        // Direct proof timeout. Retry within the same lineage when the parent is still
        // referenceable: the anchor sentinel always is, and an index-addressed parent only
        // while it sits strictly above the anchor.
        let lineage_extendable =
            found.parent.is_anchor() || found.parent.l2_block_number > anchor_l2_block;
        if lineage_extendable {
            return NextProposalAction::RetryTimedOut {
                proposal: Proposal {
                    parent_index: found.parent.parent_index,
                    parent_ref: found.parent.address,
                    root_claim,
                    l2_block_number,
                    attempt: found.attempt + U256::from(1),
                },
                invalidated_game: found.game,
            };
        }

        // The timed-out lineage referenced a parent that has since become the anchor; rebase
        // onto the sentinel. Continue that lineage's attempt chain if it already exists.
        let sentinel_attempt = found_games
            .iter()
            .find(|other| other.parent.is_anchor())
            .map_or(U256::ZERO, |other| other.attempt + U256::from(1));
        let proposal = Proposal {
            parent_index: ANCHOR_PARENT_INDEX,
            parent_ref: propose_parent.address,
            root_claim,
            l2_block_number,
            attempt: sentinel_attempt,
        };
        if sentinel_attempt == U256::ZERO {
            NextProposalAction::Propose(proposal)
        } else {
            NextProposalAction::RetryTimedOut {
                proposal,
                invalidated_game: found.game,
            }
        }
    }

    /// Returns the highest-attempt game for the transition under `parent`, if any exists.
    async fn latest_attempt(
        &self,
        parent: ParentRef,
        root_claim: alloy_primitives::B256,
        l2_block_number: u64,
    ) -> Result<Option<TransitionGame>, ProposerError> {
        let mut latest = None;
        for attempt in 0..MAX_ATTEMPT_PROBES {
            let attempt = U256::from(attempt);
            let probe = Proposal {
                parent_index: parent.parent_index,
                parent_ref: parent.address,
                root_claim,
                l2_block_number,
                attempt,
            };
            match self.execution_provider.find_game(&probe).await? {
                Some(game) => {
                    latest = Some(TransitionGame {
                        parent,
                        game,
                        attempt,
                    });
                }
                None => break,
            }
        }
        Ok(latest)
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

    /// Advances the anchor to the highest finalized game once the registry's finality airgap
    /// has elapsed for it.
    pub async fn advance_anchor(
        &self,
        finalized_games: FinalizedGames,
    ) -> Result<(), ProposerError> {
        // games are ordered by l2 block number, therefore taking the last one
        // means taking the one with the highest l2 block number
        let maybe_highest_finalized_game = finalized_games.last();
        if let Some(highest_finalized_game) = maybe_highest_finalized_game {
            // `closeGame` reverts until `disputeGameFinalitySeconds` have elapsed after
            // resolution; skip quietly until the registry considers the game finalized.
            if !self
                .execution_provider
                .is_game_finalized(highest_finalized_game.address)
                .await?
            {
                info!(
                    game_address = %highest_finalized_game.address,
                    l2_block_number = highest_finalized_game.l2_block_number,
                    "highest finalized game is still inside the finality airgap; deferring closeGame"
                );
                return Ok(());
            }
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
            parent_index = %proposal.parent_index,
            attempt = %proposal.attempt,
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
