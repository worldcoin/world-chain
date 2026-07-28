use std::time::{SystemTime, UNIX_EPOCH};

use alloy_primitives::U256;
use tracing::{info, warn};

use crate::{BondManagerClient, BondManagerConfig, ChallengerError, OwnedGames};

/// Discovers challenger-owned games and withdraws their resolved bond credits.
#[derive(Debug)]
pub struct BondManager<E> {
    config: BondManagerConfig,
    execution_provider: E,
    owned_games: OwnedGames,
    next_game_index: Option<u64>,
}

impl<E> BondManager<E> {
    /// Creates an empty bond manager. Its bounded recovery window is scanned on the first pass.
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

    /// Advances the two-phase bond claim on owned games and prunes fully settled ones.
    ///
    /// `MultiProofGame.claimCredit` first unlocks the credit in `DelayedWETH` and only
    /// transfers it on a second call after the WETH delay, so a game stays owned until its
    /// pending withdrawal is drained. Dropping it after the unlock would strand the bond.
    pub async fn withdraw_credits(&self) -> Result<(), ChallengerError> {
        let games = self.owned_games.snapshot();
        let now = unix_now();

        for game in games {
            let result: Result<bool, ChallengerError> = async {
                let status = self.execution_provider.resolution_status(game).await?;
                if !status.is_resolved() {
                    return Ok(false);
                }
                // `claimCredit` calls `closeGame`, which reverts until the registry's finality
                // airgap has elapsed.
                if !self.execution_provider.is_game_finalized(game).await? {
                    return Ok(false);
                }

                let credit = self.execution_provider.credit(game).await?;
                if credit > U256::ZERO {
                    let submission = self.execution_provider.claim_credit(game).await?;
                    info!(
                        game_address = %game,
                        tx_hash = ?submission.tx_hash,
                        amount = ?submission.amount,
                        "unlocked challenger bond credits in DelayedWETH"
                    );
                    // Keep tracking: the unlocked amount still needs the second claim.
                    return Ok(false);
                }

                let pending = self.execution_provider.pending_withdrawal(game).await?;
                if pending.amount.is_zero() {
                    // Nothing owed on this game; stop tracking it.
                    return Ok(true);
                }
                if now < pending.unlock_at {
                    return Ok(false);
                }

                let submission = self.execution_provider.claim_credit(game).await?;
                info!(
                    game_address = %game,
                    tx_hash = ?submission.tx_hash,
                    amount = ?submission.amount,
                    "withdrew challenger bond credits"
                );
                Ok(true)
            }
            .await;

            match result {
                Ok(true) => self.owned_games.remove(game),
                Ok(false) => {}
                Err(error) => {
                    warn!(%error, game_address = %game, "failed to process challenger bond credits");
                }
            }
        }

        Ok(())
    }

    /// Runs recent-game discovery and withdrawals forever.
    pub async fn run_forever(&mut self) -> Result<(), ChallengerError> {
        self.config.validate()?;

        let mut interval = tokio::time::interval(self.config.poll_interval);
        loop {
            interval.tick().await;
            if let Err(error) = self.scan_games().await {
                warn!(%error, "bond-manager game scan failed; retrying on next tick");
            }
            if let Err(error) = self.withdraw_credits().await {
                warn!(%error, "bond-manager withdrawal pass failed; retrying on next tick");
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
