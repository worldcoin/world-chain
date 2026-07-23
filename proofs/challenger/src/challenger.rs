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
use world_chain_proofs::{ConsensusProvider, RootState};

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

    #[cfg(test)]
    pub(crate) fn retry_games(&self) -> Vec<Address> {
        self.retry_games.keys().copied().collect()
    }

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
    async fn first_unexpired_game_index(
        &self,
        game_count: u64,
        now: u64,
    ) -> Result<u64, ChallengerError> {
        let mut low = 0;
        let mut high = game_count;

        while low < high {
            let middle = low + (high - low) / 2;
            let game = self.execution_provider.game_address_at(middle).await?;
            let deadline = self.execution_provider.challenge_deadline(game).await?;
            if deadline <= now {
                low = middle + 1;
            } else {
                high = middle;
            }
        }

        Ok(low)
    }

    async fn process_game(
        &self,
        game: &GameMetadata,
        latest_finalized_l2_block: BlockNumber,
        now: u64,
    ) -> Result<GameScanOutcome, GameScanError> {
        let address = game.address;
        let root_state = self
            .execution_provider
            .root_state(address)
            .await
            .map_err(|error| GameScanError {
                error,
                challenge_deadline: None,
            })?;
        if root_state != RootState::Proposed {
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
                    warn!(game = %game.address, error = %error.error, "{failure_message}");
                    self.queue_retry_game(game, error.challenge_deadline);
                }
            }
        }

        challenge_games.sort_by_key(|(_game, challenge_deadline)| *challenge_deadline);
        for (game, challenge_deadline) in challenge_games {
            match self
                .execution_provider
                .submit_challenge(game.address, self.config.challenger_bond)
                .await
            {
                Ok(submission) => {
                    self.retry_games.remove(&game.address);
                    self.owned_games.insert(game.address);
                    info!(
                        game_address = %game.address,
                        tx_hash = ?submission.tx_hash,
                        "challenged invalid World Chain proof-system game"
                    );
                }
                Err(error) => {
                    warn!(
                        game = %game.address,
                        %error,
                        "challenge submission failed; adding to retry list"
                    );
                    self.queue_retry_game(game, Some(challenge_deadline));
                }
            }
        }
    }

    /// Scans one bounded factory range and retries transient validation failures.
    pub async fn scan_once(&mut self) -> Result<(), ChallengerError> {
        self.config.validate()?;

        let now = unix_now();
        let game_count = self.execution_provider.game_count().await?;
        if self
            .next_game_index
            .is_none_or(|next_game_index| next_game_index > game_count)
        {
            let first_unexpired = self.first_unexpired_game_index(game_count, now).await?;
            info!(
                first_unexpired_game_index = first_unexpired,
                game_count, "initialized challenger game cursor"
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
            let metadata = self.execution_provider.game_metadata(game).await?;
            if !self.retry_games.contains_key(&game) {
                new_games.push(metadata);
            }
        }

        if new_games.is_empty() && self.retry_games.is_empty() {
            self.next_game_index = Some(end);
            return Ok(());
        }

        let latest_finalized_l2_block = self.consensus_provider.latest_l2_finalized_block().await?;
        let mut retry_games: Vec<RetryGame> = self.retry_games.values().copied().collect();
        retry_games.sort_by_key(|retry| retry.challenge_deadline.unwrap_or(0));
        retry_games.retain(|retry_game| {
            if retry_game
                .challenge_deadline
                .is_some_and(|challenge_deadline| now >= challenge_deadline)
            {
                warn!(game = %retry_game.game.address, "dropping retry game after challenge deadline");
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

        let scan_results = self
            .process_games(new_games, latest_finalized_l2_block, now)
            .await;
        self.handle_game_results(scan_results, "game scan failed; adding to retry list")
            .await;

        self.next_game_index = Some(end);
        Ok(())
    }

    /// Runs the challenger forever, logging transient failures and retrying on each tick.
    pub async fn run_forever(&mut self) -> Result<(), ChallengerError> {
        self.config.validate()?;

        let mut interval = tokio::time::interval(self.config.poll_interval);
        loop {
            interval.tick().await;
            if let Err(error) = self.scan_once().await {
                warn!(%error, "scan attempt failed");
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
