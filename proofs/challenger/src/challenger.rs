use crate::{
    config::ChallengerConfig,
    error::{ChallengerError, GameScanError},
    traits::ChallengerClient,
    types::{GameMetadata, GameScanOutcome, OwnedGames, RetryGame},
};
use alloy_primitives::{Address, BlockNumber};
use futures_util::{StreamExt, stream};
use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};
use tracing::{info, warn};
use world_chain_proofs::ConsensusProvider;

/// World Chain output-root challenger.
#[derive(Debug)]
pub struct WorldChainChallenger<E, C> {
    config: ChallengerConfig,
    execution_provider: E,
    consensus_provider: C,
    next_game_index: Option<u64>,
    retry_games: HashMap<Address, RetryGame>,
    owned_games: OwnedGames,
}

impl<E, C> WorldChainChallenger<E, C> {
    /// Creates a challenger with a private owned-game registry.
    pub fn new(config: ChallengerConfig, execution_provider: E, consensus_provider: C) -> Self {
        Self::with_owned_games(
            config,
            execution_provider,
            consensus_provider,
            OwnedGames::default(),
        )
    }

    /// Creates a challenger sharing owned games with lifecycle managers.
    pub fn with_owned_games(
        config: ChallengerConfig,
        execution_provider: E,
        consensus_provider: C,
        owned_games: OwnedGames,
    ) -> Self {
        Self {
            config,
            execution_provider,
            consensus_provider,
            next_game_index: None,
            retry_games: HashMap::default(),
            owned_games,
        }
    }

    /// Returns the challenger configuration.
    #[must_use]
    pub const fn config(&self) -> &ChallengerConfig {
        &self.config
    }

    /// Returns the next factory game index to scan, once initialized.
    #[must_use]
    pub const fn next_game_index(&self) -> Option<u64> {
        self.next_game_index
    }

    /// Returns the games currently queued for retry.
    #[cfg(test)]
    pub(crate) fn retry_games(&self) -> Vec<Address> {
        self.retry_games.keys().copied().collect()
    }

    /// Adds a failed game scan to the retry queue.
    fn queue_retry_game(&mut self, game: GameMetadata, challenge_deadline: Option<u64>) {
        let existing = self.retry_games.get(&game.address);
        let retry_game = RetryGame {
            game,
            challenge_deadline: challenge_deadline
                .or(existing.and_then(|retry| retry.challenge_deadline)),
            attempts: existing.map_or(1, |retry| retry.attempts.saturating_add(1)),
        };
        self.retry_games.insert(game.address, retry_game);
    }
}

impl<E, C> WorldChainChallenger<E, C>
where
    E: ChallengerClient,
    C: ConsensusProvider,
{
    /// Binary-searches the factory's monotonic challenge deadline for the oldest game
    /// that is still challengeable.
    async fn first_recent_game_index(
        &self,
        game_count: u64,
        now: u64,
    ) -> Result<u64, ChallengerError> {
        let mut low = 0;
        let mut high = game_count;

        while low < high {
            let middle = low + (high - low) / 2;
            let Some(game) = self.execution_provider.game_address_at(middle).await? else {
                // the game at this index is not a wip1006 game, which means it's an old game, advance iterator
                low = middle + 1;
                continue;
            };
            let deadline = self.execution_provider.challenge_deadline(game).await?;
            if deadline < now {
                low = middle + 1;
            } else {
                high = middle;
            }
        }

        Ok(low)
    }

    /// Determines whether a game should be challenged.
    async fn process_game(
        &self,
        game: &GameMetadata,
        latest_finalized_l2_block: BlockNumber,
        now: u64,
    ) -> Result<GameScanOutcome, GameScanError> {
        let address = game.address;
        let claim_data = self
            .execution_provider
            .claim_data(address)
            .await
            .map_err(|error| GameScanError {
                error,
                challenge_deadline: None,
            })?;
        if !claim_data.status.is_unchallenged() {
            return Ok(GameScanOutcome::Skip);
        }

        let challenge_deadline = self
            .execution_provider
            .challenge_deadline(address)
            .await
            .map_err(|error| GameScanError {
                error,
                challenge_deadline: None,
            })?;
        if now >= challenge_deadline {
            return Ok(GameScanOutcome::Skip);
        }

        if game.l2_block_number > latest_finalized_l2_block {
            return Err(GameScanError {
                error: ChallengerError::L2BlockNotFinalized {
                    game: address,
                    latest_finalized: latest_finalized_l2_block,
                    given_block: game.l2_block_number,
                },
                challenge_deadline: Some(challenge_deadline),
            });
        }

        match self
            .consensus_provider
            .output_root_at_block(game.l2_block_number)
            .await
        {
            Ok(root) if root != game.root_claim => {
                Ok(GameScanOutcome::NeedsChallenge { challenge_deadline })
            }
            Ok(_root) => Ok(GameScanOutcome::Valid),
            Err(error) => Err(GameScanError {
                error: ChallengerError::OutputRoot(error),
                challenge_deadline: Some(challenge_deadline),
            }),
        }
    }

    /// Processes games concurrently up to the configured limit.
    async fn process_games(
        &self,
        games: impl IntoIterator<Item = GameMetadata>,
        latest_finalized_l2_block: BlockNumber,
        now: u64,
    ) -> Vec<(GameMetadata, Result<GameScanOutcome, GameScanError>)> {
        stream::iter(games)
            .map(|game| async move {
                let result = self
                    .process_game(&game, latest_finalized_l2_block, now)
                    .await;
                (game, result)
            })
            .buffer_unordered(self.config.max_game_concurrency)
            .collect()
            .await
    }

    /// Handles scan outcomes and submits required challenges.
    async fn handle_game_results(
        &mut self,
        results: Vec<(GameMetadata, Result<GameScanOutcome, GameScanError>)>,
        failure_message: &'static str,
    ) {
        let mut challenge_games = Vec::new();

        for (game, result) in results {
            match result {
                Ok(GameScanOutcome::NeedsChallenge { challenge_deadline }) => {
                    challenge_games.push((game, challenge_deadline));
                }
                Ok(_outcome) => {
                    self.retry_games.remove(&game.address);
                }
                Err(error) => {
                    warn!(game_address = %game.address, error = %error.error, "{failure_message}");
                    self.queue_retry_game(game, error.challenge_deadline);
                }
            }
        }

        challenge_games.sort_by_key(|(_game, challenge_deadline)| *challenge_deadline);
        for (game, challenge_deadline) in challenge_games {
            match self.execution_provider.submit_challenge(game.address).await {
                Ok(submission) => {
                    self.retry_games.remove(&game.address);
                    self.owned_games.insert(game.address);
                    world_chain_proof_metrics::increment_challenges_submitted();
                    info!(
                        lifecycle_event = "challenge_submitted",
                        game_address = %game.address,
                        tx_hash = ?submission.tx_hash,
                        bond = ?submission.bond,
                        "challenged invalid World Chain proof-system game"
                    );
                }
                Err(error) => {
                    warn!(
                        game_address = %game.address,
                        %error,
                        "challenge submission failed; adding to retry list"
                    );
                    self.queue_retry_game(game, Some(challenge_deadline));
                }
            }
        }
    }

    /// Selects the factory index range to scan this tick.
    pub async fn select_range(&mut self, now: u64) -> Result<(u64, u64), ChallengerError> {
        let game_count = self.execution_provider.game_count().await?;
        let initialize_cursor = self
            .next_game_index
            .is_none_or(|next_game_index| next_game_index > game_count);
        if initialize_cursor {
            let first_recent = self.first_recent_game_index(game_count, now).await?;
            info!(
                first_recent_game_index = first_recent,
                game_count, now, "initialized challenger game cursor"
            );
            self.next_game_index = Some(first_recent);
        }

        let cursor = self.next_game_index.unwrap_or(game_count);
        // Factory entries are read at the finalized block, but mutable game state is read at
        // latest. Reconsider a bounded overlap so a shallow reorg of that state cannot make a
        // transient Skip outcome permanent. The overlap does not consume the new-game budget.
        let start = if initialize_cursor {
            cursor
        } else {
            cursor.saturating_sub(self.config.game_scan_lookback)
        };
        let end = cursor
            .saturating_add(self.config.max_games_per_tick)
            .min(game_count);

        Ok((start, end))
    }

    /// Loads metadata for newly discovered challenger games.
    pub async fn discover_new_games(
        &self,
        start: u64,
        end: u64,
    ) -> Result<Vec<GameMetadata>, ChallengerError> {
        let mut new_games = Vec::with_capacity((end - start) as usize);
        for index in start..end {
            // The dispute-game factory indexes every game type; skip the ones that are not ours.
            let Some(game) = self.execution_provider.game_address_at(index).await? else {
                continue;
            };
            if self.retry_games.contains_key(&game) {
                continue;
            }
            new_games.push(self.execution_provider.game_metadata(game).await?);
        }

        Ok(new_games)
    }

    /// Reprocesses games queued after transient failures.
    pub async fn handle_retry_games(
        &mut self,
        now: u64,
        latest_finalized_l2_block: BlockNumber,
    ) -> Result<(), ChallengerError> {
        let mut retry_games: Vec<RetryGame> = self.retry_games.values().copied().collect();
        retry_games.sort_by_key(|retry| retry.challenge_deadline.unwrap_or(0));
        retry_games.retain(|retry_game| {
            if retry_game
                .challenge_deadline
                .is_some_and(|challenge_deadline| now >= challenge_deadline)
            {
                warn!(game_address = %retry_game.game.address, "dropping retry game after challenge deadline");
                self.retry_games.remove(&retry_game.game.address);
                return false;
            }
            true
        });

        let retry_results = self
            .process_games(
                retry_games.into_iter().map(|retry_game| retry_game.game),
                latest_finalized_l2_block,
                now,
            )
            .await;
        self.handle_game_results(retry_results, "retry game failed")
            .await;

        Ok(())
    }

    /// Processes games discovered in the selected scan range.
    pub async fn handle_new_games(
        &mut self,
        new_games: Vec<GameMetadata>,
        now: u64,
        latest_finalized_l2_block: BlockNumber,
    ) -> Result<(), ChallengerError> {
        let scan_results = self
            .process_games(new_games, latest_finalized_l2_block, now)
            .await;
        self.handle_game_results(scan_results, "game scan failed; adding to retry list")
            .await;

        Ok(())
    }

    /// Runs one challenger iteration at the given Unix timestamp.
    pub async fn tick_at(&mut self, now: u64) -> Result<(), ChallengerError> {
        self.config.validate()?;
        let (start, end) = self.select_range(now).await?;
        let new_games = self.discover_new_games(start, end).await?;
        if new_games.is_empty() && self.retry_games.is_empty() {
            self.next_game_index = Some(end);
            return Ok(());
        }
        let latest_finalized_l2_block = self.consensus_provider.latest_l2_finalized_block().await?;
        self.handle_retry_games(now, latest_finalized_l2_block)
            .await?;
        self.handle_new_games(new_games, now, latest_finalized_l2_block)
            .await?;
        self.next_game_index = Some(end);
        Ok(())
    }

    /// Runs one challenger iteration.
    pub async fn tick(&mut self) -> Result<(), ChallengerError> {
        let now = unix_now();
        self.tick_at(now).await
    }

    /// Runs the challenger forever, logging transient failures and retrying on each tick.
    pub async fn run_forever(&mut self) -> Result<(), ChallengerError> {
        self.config.validate()?;

        let mut interval = tokio::time::interval(self.config.poll_interval);
        loop {
            interval.tick().await;
            if let Err(error) = self.tick().await {
                warn!(%error, "challenger iteration failed; retrying on next tick");
            }
        }
    }
}

/// Returns the current Unix timestamp in seconds.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs()
}
