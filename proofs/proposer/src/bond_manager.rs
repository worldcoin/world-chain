use std::collections::HashSet;

use alloy_primitives::{Address, U256};
use tracing::{info, warn};

use crate::{BondManagerClient, BondManagerConfig, ProposerError};

/// Discovers games created by the proposer and asynchronously withdraws resolved bond credits.
#[derive(Debug)]
pub struct BondManager<E> {
    config: BondManagerConfig,
    execution_provider: E,
    proposed_games: HashSet<Address>,
    next_game_index: Option<u64>,
}

impl<E> BondManager<E> {
    /// Creates an empty bond manager. The initial game window is discovered on its first scan.
    pub fn new(config: BondManagerConfig, execution_provider: E) -> Self {
        Self {
            config,
            execution_provider,
            proposed_games: HashSet::default(),
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

    /// Returns whether a game is currently awaiting resolution or withdrawal.
    #[must_use]
    pub fn tracks_game(&self, game: Address) -> bool {
        self.proposed_games.contains(&game)
    }
}

impl<E> BondManager<E>
where
    E: BondManagerClient,
{
    /// Scans the initial bounded factory window or all games appended since the last scan.
    ///
    /// The cursor advances only after the complete range succeeds. Games inserted before a
    /// partial failure are harmlessly deduplicated when that range is retried.
    pub async fn scan_games(&mut self) -> Result<(), ProposerError> {
        self.config.validate()?;

        let game_count = self.execution_provider.game_count().await?;
        let start = match self.next_game_index {
            Some(next_game_index) if next_game_index <= game_count => next_game_index,
            Some(_) | None => game_count.saturating_sub(self.config.initial_scan_limit),
        };
        let proposer = self.execution_provider.proposer_address();

        for index in start..game_count {
            // The dispute-game factory indexes every game type; skip the ones that are not ours.
            let Some(game) = self.execution_provider.game_at(index).await? else {
                continue;
            };
            if self.execution_provider.game_creator(game).await? == proposer {
                self.proposed_games.insert(game);
            }
        }

        self.next_game_index = Some(game_count);
        info!(
            start_game_index = start,
            next_game_index = game_count,
            tracked_games = self.proposed_games.len(),
            "scanned proposer bond games"
        );
        Ok(())
    }

    /// Advances the two-phase bond claim on tracked games and prunes fully settled ones.
    ///
    /// `MultiProofGame.claimCredit` first unlocks the credit in `DelayedWETH` and only
    /// transfers it on a second call after the WETH delay, so a game stays tracked until its
    /// pending withdrawal is drained. Dropping it after the unlock would strand the bond.
    pub async fn withdraw_credits(&mut self) -> Result<(), ProposerError> {
        let proposed_games: Vec<_> = self.proposed_games.iter().copied().collect();
        let now = self.execution_provider.latest_l1_timestamp().await?;

        for game in proposed_games {
            let result: Result<bool, ProposerError> = async {
                let resolution_status = self.execution_provider.resolution_status(game).await?;
                if !resolution_status.is_resolved() {
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
                        tx_hash = ?submission.tx_hash,
                        amount = ?submission.amount,
                        game_address = %game,
                        "unlocked proposer bond credits in DelayedWETH"
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
                    tx_hash = ?submission.tx_hash,
                    amount = ?submission.amount,
                    game_address = %game,
                    "withdrew proposer bond credits"
                );
                Ok(true)
            }
            .await;

            match result {
                Ok(true) => {
                    self.proposed_games.remove(&game);
                }
                Ok(false) => {}
                Err(error) => {
                    warn!(
                        %error,
                        game_address = %game,
                        "failed to process proposer credits"
                    );
                }
            }
        }

        Ok(())
    }

    /// Runs game discovery and withdrawals forever on an interval independent of proposals.
    pub async fn run_forever(&mut self) -> Result<(), ProposerError> {
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
