use crate::{
    config::DefenderConfig,
    error::DefenderError,
    game::{GameEvaluator, GameObservation},
    lane::{DEFENDED_LANE_COUNT, DEFENDED_LANES, LaneDriver, LaneState},
    traits::DefenderClient,
    types::GameMetadata,
};
use alloy_primitives::Address;
use futures_util::{StreamExt, stream};
use std::{
    collections::{HashMap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};
use tracing::{error, info, warn};
use world_chain_proofs::{
    ConsensusProvider, InvalidationReason, ProofLane, proof_count, select_lineage,
};
use world_chain_prover_service::ProofRequester;

/// An active proof-support workflow for a selected game.
#[derive(Debug, Clone, Copy)]
struct ActiveDefense {
    game: GameMetadata,
    /// Lane progress, indexed like [`DEFENDED_LANES`].
    lanes: [LaneState; DEFENDED_LANE_COUNT],
}

impl ActiveDefense {
    const fn new(game: GameMetadata) -> Self {
        Self {
            game,
            lanes: [LaneState::Pending; DEFENDED_LANE_COUNT],
        }
    }
}

/// Result of advancing a single defense for one tick.
#[derive(Debug, Clone, Copy)]
enum DefenseProgress {
    Closed,
    Complete,
    DeadlineElapsed,
    Lanes {
        lanes: [LaneState; DEFENDED_LANE_COUNT],
        sufficient_support_submitted: bool,
    },
}

fn effective_proof_bitmap(mut proof_bitmap: u8, lanes: &[LaneState; DEFENDED_LANE_COUNT]) -> u8 {
    for (slot, (proof_lane, _)) in DEFENDED_LANES.into_iter().enumerate() {
        if lanes[slot] == LaneState::Proven {
            proof_bitmap |= proof_lane.mask();
        }
    }
    proof_bitmap
}

/// Supplies proof support for every valid game on the proposer-selected lineage.
#[derive(Debug)]
pub struct WorldChainDefender<E, C, P> {
    config: DefenderConfig,
    execution_provider: E,
    consensus_provider: C,
    proof_requester: P,
    active_defenses: HashMap<Address, ActiveDefense>,
    /// Selected games whose prover retries were exhausted, mapped to their proof deadline.
    abandoned_defenses: HashMap<Address, u64>,
}

impl<E, C, P> WorldChainDefender<E, C, P> {
    /// Creates a defender from execution, consensus and prover-requester clients.
    pub fn new(
        config: DefenderConfig,
        execution_provider: E,
        consensus_provider: C,
        proof_requester: P,
    ) -> Self {
        Self {
            config,
            execution_provider,
            consensus_provider,
            proof_requester,
            active_defenses: HashMap::default(),
            abandoned_defenses: HashMap::default(),
        }
    }

    /// Returns the defender configuration.
    #[must_use]
    pub const fn config(&self) -> &DefenderConfig {
        &self.config
    }

    #[cfg(test)]
    pub(crate) fn active_defenses(&self) -> Vec<Address> {
        self.active_defenses.keys().copied().collect()
    }

    #[cfg(test)]
    pub(crate) fn abandoned_defenses(&self) -> Vec<Address> {
        self.abandoned_defenses.keys().copied().collect()
    }
}

impl<E, C, P> WorldChainDefender<E, C, P>
where
    E: DefenderClient,
    C: ConsensusProvider,
    P: ProofRequester + Sync,
{
    /// Reconstructs the valid lineage selected by the same transition rule as the proposer.
    async fn selected_lineage(&self) -> Result<Vec<GameMetadata>, DefenderError> {
        let lineage = select_lineage(&self.execution_provider, &self.consensus_provider).await?;
        let mut games = Vec::with_capacity(lineage.games().len());

        for selected in lineage.games() {
            let metadata = self
                .execution_provider
                .game_metadata(selected.game.address)
                .await?;
            games.push(metadata);
        }

        Ok(games)
    }

    async fn sync_defenses_with_selected_lineage(
        &mut self,
        selected: Vec<GameMetadata>,
        now: u64,
    ) -> Result<(), DefenderError> {
        let selected_addresses: HashSet<_> = selected.iter().map(|game| game.address).collect();
        self.active_defenses.retain(|game, _| {
            let keep = selected_addresses.contains(game);
            if !keep {
                warn!(
                    lifecycle_event = "defense_stopped",
                    outcome = "lineage_changed",
                    game_address = %game,
                    "stopping proof support because game left the selected lineage"
                );
            }
            keep
        });
        self.abandoned_defenses
            .retain(|game, deadline| selected_addresses.contains(game) && now < *deadline);

        let evaluator = GameEvaluator::new(&self.execution_provider);
        for game in selected {
            if self.active_defenses.contains_key(&game.address)
                || self.abandoned_defenses.contains_key(&game.address)
            {
                continue;
            }
            if evaluator.needs_defense(&game, now).await? {
                info!(
                    lifecycle_event = "defense_started",
                    game_address = %game.address,
                    l2_block_number = game.l2_block_number,
                    challenge_deadline = game.challenge_deadline,
                    proof_deadline = game.proof_deadline,
                    "selected game needs proof support; starting proof workflow"
                );
                self.active_defenses
                    .insert(game.address, ActiveDefense::new(game));
            }
        }
        Ok(())
    }

    async fn advance_defense(
        &self,
        defense: &ActiveDefense,
        now: u64,
    ) -> Result<DefenseProgress, DefenderError> {
        let metadata = &defense.game;
        let evaluator = GameEvaluator::new(&self.execution_provider);
        let (proof_bitmap, deadline, required_support, tee_only) =
            match evaluator.observe(metadata).await? {
                GameObservation::Finalized => return Ok(DefenseProgress::Complete),
                GameObservation::Invalidated { reason } => {
                    if reason == InvalidationReason::ProofTimeout {
                        return Ok(DefenseProgress::DeadlineElapsed);
                    }
                    return Ok(DefenseProgress::Closed);
                }
                GameObservation::Proposed {
                    proof_bitmap,
                    has_initial_support,
                } => {
                    if has_initial_support {
                        return Ok(DefenseProgress::Complete);
                    }
                    (proof_bitmap, metadata.challenge_deadline, 1, true)
                }
                GameObservation::Challenged {
                    proof_bitmap,
                    has_required_support,
                } => {
                    if has_required_support {
                        return Ok(DefenseProgress::Complete);
                    }
                    (
                        proof_bitmap,
                        metadata.proof_deadline,
                        metadata.proof_threshold,
                        false,
                    )
                }
            };
        if now >= deadline {
            return Ok(DefenseProgress::DeadlineElapsed);
        }

        let mut lanes = defense.lanes;
        let mut lanes_needed = required_support
            .saturating_sub(proof_count(effective_proof_bitmap(proof_bitmap, &lanes)));
        let lane_driver = LaneDriver::new(&self.execution_provider, &self.proof_requester);
        for (slot, (proof_lane, backend)) in DEFENDED_LANES.into_iter().enumerate() {
            if proof_bitmap & proof_lane.mask() != 0 {
                lanes[slot] = LaneState::Proven;
                continue;
            }
            if tee_only && proof_lane != ProofLane::TeeAttestation {
                continue;
            }
            if lanes[slot] == LaneState::Proven || lanes_needed == 0 {
                continue;
            }
            lanes[slot] = lane_driver
                .advance(metadata, proof_lane, backend, lanes[slot])
                .await;
            // An active request reserves one of the remaining support slots so a lower-priority
            // lane is not started unless this lane is permanently abandoned.
            if lanes[slot] != LaneState::Abandoned {
                lanes_needed -= 1;
            }
        }

        Ok(DefenseProgress::Lanes {
            lanes,
            sufficient_support_submitted: proof_count(effective_proof_bitmap(proof_bitmap, &lanes))
                >= required_support,
        })
    }

    async fn scan_active_defenses(
        &self,
        now: u64,
    ) -> Vec<(ActiveDefense, Result<DefenseProgress, DefenderError>)> {
        stream::iter(self.active_defenses.values().copied().collect::<Vec<_>>())
            .map(|defense| async move {
                let result = self.advance_defense(&defense, now).await;
                (defense, result)
            })
            .buffer_unordered(self.config.max_game_concurrency)
            .collect()
            .await
    }

    fn handle_defense_progress(
        &mut self,
        results: Vec<(ActiveDefense, Result<DefenseProgress, DefenderError>)>,
    ) {
        for (defense, result) in results {
            let game = defense.game.address;
            match result {
                Ok(DefenseProgress::Closed) => {
                    info!(
                        lifecycle_event = "defense_stopped",
                        outcome = "game_closed",
                        game_address = %game,
                        "game no longer needs proof support; defense closed"
                    );
                    self.active_defenses.remove(&game);
                }
                Ok(DefenseProgress::Complete) => {
                    info!(
                        lifecycle_event = "defense_completed",
                        outcome = "sufficient_support",
                        game_address = %game,
                        "game has sufficient proof support; defense completed"
                    );
                    self.active_defenses.remove(&game);
                }
                Ok(DefenseProgress::DeadlineElapsed) => {
                    error!(
                        lifecycle_event = "defense_failed",
                        outcome = "deadline_elapsed",
                        game_address = %game,
                        challenge_deadline = defense.game.challenge_deadline,
                        proof_deadline = defense.game.proof_deadline,
                        "game proof deadline elapsed before proof support completed"
                    );
                    self.active_defenses.remove(&game);
                }
                Ok(DefenseProgress::Lanes {
                    lanes,
                    sufficient_support_submitted,
                }) => {
                    if lanes.iter().all(|lane| *lane == LaneState::Proven) {
                        info!(
                            lifecycle_event = "defense_completed",
                            outcome = "all_lanes_submitted",
                            game_address = %game,
                            "all proof lanes submitted; defense completed"
                        );
                        self.active_defenses.remove(&game);
                    } else if sufficient_support_submitted {
                        if let Some(defense) = self.active_defenses.get_mut(&game) {
                            defense.lanes = lanes;
                        }
                    } else if lanes.iter().all(|lane| lane.is_terminal()) {
                        error!(
                            lifecycle_event = "defense_failed",
                            outcome = "lanes_abandoned",
                            game_address = %game,
                            "defense abandoned without proving all lanes"
                        );
                        self.abandoned_defenses
                            .insert(game, defense.game.proof_deadline);
                        self.active_defenses.remove(&game);
                    } else if let Some(defense) = self.active_defenses.get_mut(&game) {
                        defense.lanes = lanes;
                    }
                }
                Err(err) => {
                    warn!(game_address = %game, error = %err, "defense scan failed; retrying next tick");
                }
            }
        }
    }

    pub(crate) async fn tick_at(&mut self, now: u64) -> Result<(), DefenderError> {
        self.config.validate()?;
        let selected = self.selected_lineage().await?;
        self.sync_defenses_with_selected_lineage(selected, now)
            .await?;
        let progress = self.scan_active_defenses(now).await;
        self.handle_defense_progress(progress);
        Ok(())
    }

    /// Advances the defender by one polling tick.
    pub async fn tick(&mut self) -> Result<(), DefenderError> {
        self.tick_at(unix_now()).await
    }

    /// Runs the defender forever, logging transient failures and retrying on each tick.
    pub async fn run_forever(&mut self) -> Result<(), DefenderError> {
        self.config.validate()?;

        let mut interval = tokio::time::interval(self.config.poll_interval);
        loop {
            interval.tick().await;
            if let Err(e) = self.tick().await {
                warn!(%e, "defender iteration failed; retrying on next tick");
            }
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs()
}
