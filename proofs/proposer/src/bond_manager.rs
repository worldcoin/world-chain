use std::collections::HashSet;

use alloy_primitives::Address;
use tracing::{info, warn};

use crate::{BondManagerClient, BondManagerConfig, ProposerError, types::ClaimOutcome};

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
            // Games of other types (e.g. the stock cannon games) are skipped.
            let Some(game) = self.execution_provider.game_at(index).await? else {
                continue;
            };
            if self.execution_provider.game_proposer(game).await? == proposer {
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

    /// Advances two-phase DelayedWETH claims and prunes games whose credits are fully settled.
    ///
    /// Each resolved game walks through: unlock (after the registry's finality airgap) →
    /// withdrawal (after the DelayedWETH delay). A game is pruned once its funds land or it
    /// holds no credit for the proposer.
    pub async fn withdraw_credits(&mut self) -> Result<(), ProposerError> {
        let proposed_games: Vec<_> = self.proposed_games.iter().copied().collect();

        for game in proposed_games {
            let result: Result<bool, ProposerError> = async {
                let resolution_status = self.execution_provider.resolution_status(game).await?;
                if !resolution_status.is_resolved() {
                    return Ok(false);
                }

                match self.execution_provider.claim_credits(game).await? {
                    ClaimOutcome::NotReady => Ok(false),
                    ClaimOutcome::Unlocked { tx_hash, amount } => {
                        info!(
                            tx_hash = ?tx_hash,
                            amount = ?amount,
                            game_address = %game,
                            "unlocked bond credits in DelayedWETH"
                        );
                        Ok(false)
                    }
                    ClaimOutcome::Claimed { tx_hash, amount } => {
                        info!(
                            tx_hash = ?tx_hash,
                            amount = ?amount,
                            game_address = %game,
                            "withdrew bond credits"
                        );
                        Ok(true)
                    }
                    ClaimOutcome::NoCredit => Ok(true),
                }
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
