use tracing::{info, warn};

use crate::{BondManagerClient, BondManagerConfig, ChallengerError, OwnedGames};

/// Discovers challenger-owned games and settles their resolved bond pots.
#[derive(Debug)]
pub struct BondManager<E> {
    config: BondManagerConfig,
    execution_provider: E,
    owned_games: OwnedGames,
    next_game_index: Option<u64>,
}

impl<E> BondManager<E> {
    /// Creates an empty bond manager. Its bounded lookback window is scanned on the first pass.
    pub const fn new(
        config: BondManagerConfig,
        execution_provider: E,
        owned_games: OwnedGames,
    ) -> Self {
        Self {
            config,
            execution_provider,
            owned_games,
            next_game_index: None,
        }
    }

    /// Returns the bond-manager configuration.
    #[must_use]
    pub const fn config(&self) -> &BondManagerConfig {
        &self.config
    }

    /// Returns the next factory game index to scan, once initialized.
    #[must_use]
    pub const fn next_game_index(&self) -> Option<u64> {
        self.next_game_index
    }
}

impl<E> BondManager<E>
where
    E: BondManagerClient,
{
    /// Scans the bounded startup window or games appended since the last scan.
    pub async fn scan_games(&mut self) -> Result<(), ChallengerError> {
        self.config.validate()?;

        let game_count = self.execution_provider.game_count().await?;
        let start = match self.next_game_index {
            Some(next_game_index) if next_game_index <= game_count => next_game_index,
            Some(_) | None => game_count.saturating_sub(self.config.initial_scan_limit),
        };
        let challenger = self.execution_provider.challenger_address();

        for index in start..game_count {
            // The dispute-game factory indexes every game type; skip the ones that are not ours.
            let Some(game) = self.execution_provider.game_address_at(index).await? else {
                continue;
            };
            if self.execution_provider.game_challenger(game).await? == challenger {
                self.owned_games.insert(game);
            }
        }

        self.next_game_index = Some(game_count);
        info!(
            start_game_index = start,
            next_game_index = game_count,
            tracked_games = self.owned_games.snapshot().len(),
            "scanned challenger bond games"
        );
        Ok(())
    }

    /// Settles finalized owned games and prunes games whose complete pot is settled.
    pub async fn settle_games(&self) -> Result<(), ChallengerError> {
        let games = self.owned_games.snapshot();

        for game in games {
            let result: Result<bool, ChallengerError> = async {
                if self.execution_provider.is_game_settled(game).await? {
                    world_chain_proof_metrics::increment_games_closed(
                        "challenger",
                        "already_settled",
                    );
                    return Ok(true);
                }
                let status = self.execution_provider.resolution_status(game).await?;
                if !status.is_resolved() {
                    return Ok(false);
                }
                if !self.execution_provider.is_game_finalized(game).await? {
                    return Ok(false);
                }

                let submission = self.execution_provider.close_game(game).await?;
                world_chain_proof_metrics::increment_games_closed("challenger", "submitted");
                info!(
                    game_address = %game,
                    tx_hash = ?submission.tx_hash,
                    "settled challenger-owned game into reusable vault balances"
                );
                Ok(true)
            }
            .await;

            match result {
                Ok(true) => self.owned_games.remove(game),
                Ok(false) => {}
                Err(error) => {
                    warn!(%error, game_address = %game, "failed to process challenger bond settlement");
                }
            }
        }

        Ok(())
    }

    /// Runs recent-game discovery and settlement forever.
    pub async fn run_forever(&mut self) -> Result<(), ChallengerError> {
        self.config.validate()?;

        let mut interval = tokio::time::interval(self.config.poll_interval);
        loop {
            interval.tick().await;
            if let Err(error) = self.scan_games().await {
                warn!(%error, "bond-manager game scan failed; retrying on next tick");
            }
            if let Err(error) = self.settle_games().await {
                warn!(%error, "bond-manager settlement pass failed; retrying on next tick");
            }
        }
    }
}
