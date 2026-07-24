use crate::{
    config::DefenderConfig,
    error::DefenderError,
    game::{GameEvaluator, GameObservation, GameOutcome},
    lane::{DEFENDED_LANE_COUNT, DEFENDED_LANES, LaneDriver, LaneState},
    traits::DefenderClient,
    types::GameMetadata,
};
use alloy_primitives::{Address, BlockNumber};
use futures_util::{StreamExt, stream};
use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};
use tracing::{error, info, warn};
use world_chain_proofs::{ConsensusProvider, InvalidationReason};
use world_chain_prover_service::ProofRequester;

/// An active defense of a challenged game with a valid root.
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
    /// The game left the `Challenged` state on-chain.
    Closed,
    /// The game already has enough proof support to complete the defense.
    Complete,
    /// The proof deadline elapsed before the defense completed.
    DeadlineElapsed,
    /// Lane progress after this tick.
    Lanes([LaneState; DEFENDED_LANE_COUNT]),
}

/// World Chain Defender.
///
/// Discovers allowlisted games through the factory index and watches them until
/// they no longer need defense. When a game is challenged and its root claim
/// matches the canonical output root, the defender requests one proof per
/// defended lane from the `prover-service` and submits each completed proof
/// on-chain via `submitProofLane`.
#[derive(Debug)]
pub struct WorldChainDefender<E, C, P> {
    config: DefenderConfig,
    execution_provider: E,
    consensus_provider: C,
    proof_requester: P,
    next_game_index: Option<u64>,
    watched_games: HashMap<Address, GameMetadata>,
    active_defenses: HashMap<Address, ActiveDefense>,
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
            next_game_index: None,
            watched_games: HashMap::default(),
            active_defenses: HashMap::default(),
        }
    }

    /// Returns the defender configuration.
    #[must_use]
    pub const fn config(&self) -> &DefenderConfig {
        &self.config
    }

    /// Returns the next factory game index to scan, once initialized.
    #[must_use]
    pub const fn next_game_index(&self) -> Option<u64> {
        self.next_game_index
    }

    #[cfg(test)]
    pub(crate) fn watched_games(&self) -> Vec<Address> {
        self.watched_games.keys().copied().collect()
    }

    #[cfg(test)]
    pub(crate) fn active_defenses(&self) -> Vec<Address> {
        self.active_defenses.keys().copied().collect()
    }
}

impl<E, C, P> WorldChainDefender<E, C, P>
where
    E: DefenderClient,
    C: ConsensusProvider,
    P: ProofRequester + Sync,
{
    async fn first_unexpired_game_index(
        &self,
        game_count: u64,
        now: u64,
    ) -> Result<u64, DefenderError> {
        let mut low = 0;
        let mut high = game_count;

        while low < high {
            let middle = low + (high - low) / 2;
            let game = self.execution_provider.game_address_at(middle).await?;
            let deadline = self.execution_provider.proof_deadline(game).await?;
            if deadline <= now {
                low = middle + 1;
            } else {
                high = middle;
            }
        }

        Ok(low)
    }

    async fn scan_games(
        &self,
        games: impl IntoIterator<Item = GameMetadata>,
        now: u64,
    ) -> Vec<(GameMetadata, Result<GameOutcome, DefenderError>)> {
        let evaluator = GameEvaluator::new(&self.execution_provider, &self.consensus_provider);
        stream::iter(games)
            .map(move |game| {
                let evaluator = evaluator;
                async move {
                    let result = evaluator.scan(&game, now).await;
                    (game, result)
                }
            })
            .buffer_unordered(self.config.max_game_concurrency)
            .collect()
            .await
    }

    fn handle_game_scan_results(
        &mut self,
        results: Vec<(GameMetadata, Result<GameOutcome, DefenderError>)>,
    ) {
        for (game, result) in results {
            match result {
                Ok(GameOutcome::Track) => {
                    self.watched_games.insert(game.address, game);
                }
                Ok(GameOutcome::Defend) => self.start_defense(game),
                Ok(GameOutcome::Drop) => {}
                Err(error) => {
                    warn!(
                        game = %game.address,
                        %error,
                        "game scan failed; retaining for monitoring"
                    );
                    self.watched_games.insert(game.address, game);
                }
            }
        }
    }

    async fn scan_watched_games(
        &self,
        latest_finalized_l2_block: BlockNumber,
        now: u64,
    ) -> Vec<(GameMetadata, Result<GameOutcome, DefenderError>)> {
        let evaluator = GameEvaluator::new(&self.execution_provider, &self.consensus_provider);
        stream::iter(self.watched_games.values().copied().collect::<Vec<_>>())
            .map(move |game| {
                let evaluator = evaluator;
                async move {
                    let result = evaluator.watch(&game, latest_finalized_l2_block, now).await;
                    (game, result)
                }
            })
            .buffer_unordered(self.config.max_game_concurrency)
            .collect()
            .await
    }

    fn handle_watch_outcomes(
        &mut self,
        results: Vec<(GameMetadata, Result<GameOutcome, DefenderError>)>,
    ) {
        for (metadata, result) in results {
            let game = metadata.address;
            match result {
                Ok(GameOutcome::Defend) => self.start_defense(metadata),
                Ok(GameOutcome::Drop) => {
                    self.watched_games.remove(&game);
                }
                Ok(GameOutcome::Track) => {}
                Err(err) => {
                    warn!(game = %game, error = %err, "game watch failed; retrying next tick");
                }
            }
        }
    }

    fn start_defense(&mut self, metadata: GameMetadata) {
        let game = metadata.address;
        info!(%game, "challenged game has a valid root; starting defense");
        self.watched_games.remove(&game);
        self.active_defenses
            .insert(game, ActiveDefense::new(metadata));
    }

    async fn advance_defense(
        &self,
        defense: &ActiveDefense,
        now: u64,
    ) -> Result<DefenseProgress, DefenderError> {
        let metadata = &defense.game;
        let evaluator = GameEvaluator::new(&self.execution_provider, &self.consensus_provider);
        let proof_bitmap = match evaluator.observe(metadata).await? {
            GameObservation::Finalized => return Ok(DefenseProgress::Complete),
            GameObservation::Invalidated(reason) => {
                if reason == InvalidationReason::ProofTimeout {
                    return Ok(DefenseProgress::DeadlineElapsed);
                }
                return Ok(DefenseProgress::Closed);
            }
            GameObservation::Challenged {
                proof_bitmap,
                has_required_support,
            } => {
                if has_required_support {
                    return Ok(DefenseProgress::Complete);
                }
                proof_bitmap
            }
            GameObservation::Unset | GameObservation::Proposed => {
                return Ok(DefenseProgress::Closed);
            }
        };
        if now >= metadata.proof_deadline {
            return Ok(DefenseProgress::DeadlineElapsed);
        }

        let mut lanes = defense.lanes;
        let lane_driver = LaneDriver::new(
            &self.execution_provider,
            &self.proof_requester,
            self.config.max_proof_attempts,
        );
        for (slot, (proof_lane, backend)) in DEFENDED_LANES.into_iter().enumerate() {
            // skip lanes already proven on-chain, by us or by anyone else
            if proof_bitmap & proof_lane.mask() != 0 {
                lanes[slot] = LaneState::Proven;
                continue;
            }
            lanes[slot] = lane_driver
                .advance(metadata, proof_lane, backend, lanes[slot])
                .await;
        }
        Ok(DefenseProgress::Lanes(lanes))
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
                    info!(%game, "game no longer needs proof support; defense closed");
                    self.active_defenses.remove(&game);
                }
                Ok(DefenseProgress::Complete) => {
                    info!(%game, "game has sufficient proof support; defense completed");
                    self.active_defenses.remove(&game);
                }
                Ok(DefenseProgress::DeadlineElapsed) => {
                    error!(
                        %game,
                        proof_deadline = defense.game.proof_deadline,
                        "challenged game proof deadline elapsed before defense completed"
                    );
                    self.active_defenses.remove(&game);
                }
                Ok(DefenseProgress::Lanes(lanes)) => {
                    if lanes.iter().all(|lane| *lane == LaneState::Proven) {
                        info!(%game, "all proof lanes submitted; defense completed");
                        self.active_defenses.remove(&game);
                    } else if lanes.iter().all(|lane| lane.is_terminal()) {
                        error!(%game, "defense abandoned without proving all lanes");
                        self.active_defenses.remove(&game);
                    } else if let Some(defense) = self.active_defenses.get_mut(&game) {
                        defense.lanes = lanes;
                    }
                }
                Err(err) => {
                    warn!(game = %game, error = %err, "defense scan failed; retrying next tick");
                }
            }
        }
    }

    async fn advance_active_defenses(&mut self, now: u64) {
        let defense_results = self.scan_active_defenses(now).await;
        self.handle_defense_progress(defense_results);
    }

    async fn discover_games(&mut self, now: u64) -> Result<(), DefenderError> {
        let game_count = self.execution_provider.game_count().await?;
        if self
            .next_game_index
            .is_none_or(|next_game_index| next_game_index > game_count)
        {
            let first_unexpired = self.first_unexpired_game_index(game_count, now).await?;
            info!(
                first_unexpired_game_index = first_unexpired,
                game_count, "initialized defender game cursor"
            );
            self.next_game_index = Some(first_unexpired);
        }

        let start = self.next_game_index.unwrap_or(game_count);
        let end = start
            .saturating_add(self.config.max_games_per_tick)
            .min(game_count);
        let mut new_games = Vec::with_capacity((end - start) as usize);
        for index in start..end {
            let game = self.execution_provider.game_address_at(index).await?;
            if self.watched_games.contains_key(&game) || self.active_defenses.contains_key(&game) {
                continue;
            }

            let proposer = self.execution_provider.game_proposer(game).await?;
            if proposer != self.config.allowed_proposer {
                continue;
            }
            new_games.push(self.execution_provider.game_metadata(game).await?);
        }

        let scan_results = self.scan_games(new_games, now).await;
        self.handle_game_scan_results(scan_results);
        self.next_game_index = Some(end);
        Ok(())
    }

    async fn advance_watched_games(&mut self, now: u64) -> Result<(), DefenderError> {
        if self.watched_games.is_empty() {
            return Ok(());
        }

        let latest_finalized_l2_block = self.consensus_provider.latest_l2_finalized_block().await?;

        let watch_results = self
            .scan_watched_games(latest_finalized_l2_block, now)
            .await;
        self.handle_watch_outcomes(watch_results);
        Ok(())
    }

    pub(crate) async fn tick_at(&mut self, now: u64) -> Result<(), DefenderError> {
        self.config.validate()?;

        self.advance_active_defenses(now).await;
        self.discover_games(now).await?;
        self.advance_watched_games(now).await
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
                warn!(%e, "scan attempt failed");
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
