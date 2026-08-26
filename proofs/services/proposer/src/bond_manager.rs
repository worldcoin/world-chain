use std::collections::HashSet;

use alloy_primitives::Address;
use tracing::{info, warn};

use crate::{BondManagerClient, BondManagerConfig, ProposerError};

/// Discovers proposer-owned games, resolves abandoned games, and settles their bond pots.
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

    /// Returns whether a game is currently awaiting resolution or settlement.
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

    /// Resolves abandoned games, settles finalized games, and prunes settled games.
    pub async fn settle_games(&mut self) -> Result<(), ProposerError> {
        let proposed_games: Vec<_> = self.proposed_games.iter().copied().collect();
        let active_domain_hash = self.execution_provider.active_domain_hash();

        for game in proposed_games {
            let result: Result<bool, ProposerError> = async {
                if self.execution_provider.is_game_settled(game).await? {
                    world_chain_proof_metrics::increment_games_closed(
                        "proposer",
                        "already_settled",
                    );
                    return Ok(true);
                }
                let resolution_status = self.execution_provider.resolution_status(game).await?;
                if !resolution_status.is_resolved() {
                    // Resolve games abandoned by retries or domain upgrades; active-domain direct
                    // outcomes stay with the proposer to avoid racing retry creation.
                    let obsolete_domain_resolvable = if resolution_status.resolvable {
                        self.execution_provider.game_domain_hash(game).await? != active_domain_hash
                    } else {
                        false
                    };
                    if resolution_status.invalid_parent_resolvable() || obsolete_domain_resolvable {
                        let submission = self.execution_provider.resolve_game(game).await?;
                        info!(
                            lifecycle_event = "game_resolved",
                            outcome = ?resolution_status.outcome,
                            invalidation_reason = ?resolution_status.invalidation_reason,
                            obsolete_domain = obsolete_domain_resolvable,
                            tx_hash = ?submission.tx_hash,
                            game_address = %game,
                            "resolved abandoned proposer-owned game"
                        );
                    }
                    return Ok(false);
                }
                if !self.execution_provider.is_game_finalized(game).await? {
                    return Ok(false);
                }

                let submission = self.execution_provider.close_game(game).await?;
                world_chain_proof_metrics::increment_games_closed("proposer", "submitted");
                info!(
                    tx_hash = ?submission.tx_hash,
                    game_address = %game,
                    "settled proposer-owned game into reusable vault balances"
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
                        "failed to process proposer bond settlement"
                    );
                }
            }
        }

        Ok(())
    }

    /// Runs game discovery and bond settlement forever on an interval independent of proposals.
    pub async fn run_forever(&mut self) -> Result<(), ProposerError> {
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
