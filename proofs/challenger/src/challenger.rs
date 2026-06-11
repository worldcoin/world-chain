use crate::{
    config::ChallengerConfig,
    error::{ChallengerError, GameScanError},
    traits::ChallengerClient,
    types::{GameScanOutcome, RetryGame},
};
use alloy_primitives::{Address, BlockNumber};
use futures_util::{StreamExt, stream};
use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};
use tracing::warn;
use world_chain_proofs::{ConsensusProvider, GameCreated, RootState};

/// The number of L1 blocks published in 24h.
const ONE_DAY_OF_L1_BLOCKS: u64 = 7_200;
/// The safety margin of blocks.
const MARGIN: u64 = 500;

/// World Chain Challenger.
#[derive(Debug)]
pub struct WorldChainChallenger<E, C> {
    config: ChallengerConfig,
    execution_provider: E,
    consensus_provider: C,
    cursor: BlockNumber,
    retry_games: HashMap<Address, RetryGame>,
}

impl<E, C> WorldChainChallenger<E, C> {
    /// Creates a challenger from contract and output-root clients.
    pub fn new(config: ChallengerConfig, execution_provider: E, consensus_provider: C) -> Self {
        Self {
            config,
            execution_provider,
            consensus_provider,
            cursor: 0,
            retry_games: HashMap::default(),
        }
    }

    /// Returns the challenger configuration.
    #[must_use]
    pub const fn config(&self) -> &ChallengerConfig {
        &self.config
    }

    #[cfg(test)]
    pub(crate) fn retry_games(&self) -> Vec<Address> {
        self.retry_games.keys().copied().collect()
    }

    fn queue_retry_game(&mut self, game_created: GameCreated, challenge_deadline: Option<u64>) {
        let existing = self.retry_games.get(&game_created.game);
        let retry_game = RetryGame {
            game_created,
            challenge_deadline: challenge_deadline
                .or(existing.and_then(|retry| retry.challenge_deadline)),
            attempts: existing.map_or(1, |retry| retry.attempts.saturating_add(1)),
        };
        self.retry_games.insert(game_created.game, retry_game);
    }
}

impl<E, C> WorldChainChallenger<E, C>
where
    E: ChallengerClient,
    C: ConsensusProvider,
{
    async fn process_game(
        &self,
        game_created: &GameCreated,
        latest_finalized_l2_block: BlockNumber,
        now: u64,
    ) -> Result<GameScanOutcome, GameScanError> {
        let game = game_created.game;
        let root_state = self
            .execution_provider
            .root_state(game)
            .await
            .map_err(|error| GameScanError {
                error,
                challenge_deadline: None,
            })?;
        if root_state != RootState::Proposed {
            // root state is not `Proposed` anymore, skip immediately
            return Ok(GameScanOutcome::Skip);
        }
        let challenge_deadline = self
            .execution_provider
            .challenge_deadline(game)
            .await
            .map_err(|error| GameScanError {
                error,
                challenge_deadline: None,
            })?;
        let challenge_deadline_value = challenge_deadline;
        let challenge_deadline = Some(challenge_deadline_value);

        if now >= challenge_deadline_value {
            // challenge deadline has expired, skip immediately
            return Ok(GameScanOutcome::Skip);
        }
        // ensure that the l2 block is finalized
        if game_created.l2_block_number > latest_finalized_l2_block {
            return Err(GameScanError {
                error: ChallengerError::L2BlockNotFinalized {
                    game,
                    latest_finalized: latest_finalized_l2_block,
                    given_block: game_created.l2_block_number,
                },
                challenge_deadline,
            });
        }

        match self
            .consensus_provider
            .output_root_at_block(game_created.l2_block_number)
            .await
        {
            Ok(root) if root != game_created.root_claim => Ok(GameScanOutcome::NeedsChallenge {
                challenge_deadline: challenge_deadline_value,
            }),
            Ok(_root) => {
                // valid root, leave it
                Ok(GameScanOutcome::Valid)
            }
            Err(err) => Err(GameScanError {
                error: ChallengerError::OutputRoot(err),
                challenge_deadline,
            }),
        }
    }

    async fn process_games(
        &self,
        games: impl IntoIterator<Item = GameCreated>,
        latest_finalized_l2_block: BlockNumber,
        now: u64,
    ) -> Vec<(GameCreated, Result<GameScanOutcome, GameScanError>)> {
        stream::iter(games)
            .map(|game_created| async move {
                let result = self
                    .process_game(&game_created, latest_finalized_l2_block, now)
                    .await;
                (game_created, result)
            })
            .buffer_unordered(self.config.max_game_concurrency)
            .collect()
            .await
    }

    async fn handle_game_results(
        &mut self,
        results: Vec<(GameCreated, Result<GameScanOutcome, GameScanError>)>,
        failure_message: &'static str,
    ) {
        let mut challenge_games = Vec::new();

        for (game_created, result) in results {
            match result {
                Ok(GameScanOutcome::NeedsChallenge { challenge_deadline }) => {
                    challenge_games.push((game_created, challenge_deadline));
                }
                Ok(_outcome) => {
                    self.retry_games.remove(&game_created.game);
                }
                Err(err) => {
                    warn!(game = %game_created.game, error = %err.error, "{failure_message}");
                    self.queue_retry_game(game_created, err.challenge_deadline);
                }
            }
        }

        challenge_games.sort_by_key(|(_game_created, challenge_deadline)| *challenge_deadline);

        for (game_created, challenge_deadline) in challenge_games {
            match self
                .execution_provider
                .submit_challenge(game_created.game, self.config.challenger_bond)
                .await
            {
                Ok(_submission) => {
                    self.retry_games.remove(&game_created.game);
                }
                Err(error) => {
                    warn!(game = %game_created.game, %error, "challenge submission failed; adding to retry list");
                    self.queue_retry_game(game_created, Some(challenge_deadline));
                }
            }
        }
    }

    pub async fn scan_once(&mut self) -> Result<(), ChallengerError> {
        let target = self.execution_provider.finalized_l1_block_num().await?;
        let from = if self.cursor == 0 {
            target.saturating_sub(ONE_DAY_OF_L1_BLOCKS + MARGIN)
        } else {
            self.cursor
        };
        let has_new_l1_blocks = from <= target;
        let mut games_created = if has_new_l1_blocks {
            self.execution_provider.games_created(from, target).await?
        } else {
            Vec::new()
        };

        if games_created.is_empty() && self.retry_games.is_empty() {
            if has_new_l1_blocks {
                self.cursor = target + 1;
            }
            return Ok(());
        }

        let latest_finalized_l2_block = self.consensus_provider.latest_l2_finalized_block().await?;
        let now = unix_now();

        let mut retry_games: Vec<RetryGame> = self.retry_games.values().copied().collect();
        // sort retry games by deadline with unknown deadlines first
        retry_games.sort_by_key(|retry| retry.challenge_deadline.unwrap_or(0));
        // retain only games that are not already in `retry_games`
        games_created.retain(|game_created| !self.retry_games.contains_key(&game_created.game));

        retry_games.retain(|retry_game| {
            let game = retry_game.game_created.game;
            if retry_game
                .challenge_deadline
                .is_some_and(|challenge_deadline| now >= challenge_deadline)
            {
                warn!(game = %game, "dropping retry game after challenge deadline");
                self.retry_games.remove(&game);
                return false;
            }

            true
        });

        let retry_results = self
            .process_games(
                retry_games
                    .into_iter()
                    .map(|retry_game| retry_game.game_created),
                latest_finalized_l2_block,
                now,
            )
            .await;
        self.handle_game_results(retry_results, "retry game failed")
            .await;

        let scan_results = self
            .process_games(games_created, latest_finalized_l2_block, now)
            .await;
        self.handle_game_results(scan_results, "game scan failed; adding to retry list")
            .await;
        // if the scan goes well, update the cursor
        if has_new_l1_blocks {
            self.cursor = target + 1;
        }
        Ok(())
    }

    /// Runs the challenger forever, logging transient failures and retrying on each tick.
    pub async fn run_forever(&mut self) -> Result<(), ChallengerError> {
        self.config.validate()?;

        let mut interval = tokio::time::interval(self.config.poll_interval);
        loop {
            interval.tick().await;
            if let Err(e) = self.scan_once().await {
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
