use tracing::{info, warn};
use world_chain_proofs::{InvalidationReason, RootState};

use crate::{ChallengerError, OwnedGames, ResolutionManagerClient, ResolutionManagerConfig};

/// Resolves games challenged by the managed challenger once their outcome is ready.
#[derive(Debug)]
pub struct ResolutionManager<E> {
    config: ResolutionManagerConfig,
    execution_provider: E,
    owned_games: OwnedGames,
}

impl<E> ResolutionManager<E> {
    /// Creates a resolution manager backed by the shared owned-game registry.
    pub const fn new(
        config: ResolutionManagerConfig,
        execution_provider: E,
        owned_games: OwnedGames,
    ) -> Self {
        Self {
            config,
            execution_provider,
            owned_games,
        }
    }

    /// Returns the resolution-manager configuration.
    #[must_use]
    pub const fn config(&self) -> &ResolutionManagerConfig {
        &self.config
    }
}

impl<E> ResolutionManager<E>
where
    E: ResolutionManagerClient,
{
    /// Resolves at most the configured number of currently resolvable owned games.
    pub async fn resolve_games(&self) -> Result<(), ChallengerError> {
        self.config.validate()?;

        let mut games = self.owned_games.snapshot();
        // keep game selection deterministic when the per-tick resolution budget is reached
        games.sort_unstable();
        let mut resolutions_submitted = 0;

        for game in games {
            if resolutions_submitted >= self.config.max_resolutions_per_tick {
                break;
            }
            let result: Result<(), ChallengerError> = async {
                let status = self.execution_provider.resolution_status(game).await?;
                if !status.resolvable {
                    return Ok(());
                }
                if status.root_state != RootState::Invalidated
                    || status.invalidation_reason != InvalidationReason::ProofTimeout
                {
                    warn!(
                        game_address = %game,
                        outcome = ?status.root_state,
                        invalidation_reason = ?status.invalidation_reason,
                        "challenger-owned invalid game has an unexpected resolvable outcome"
                    );
                }

                let submission = self.execution_provider.resolve(game).await?;
                info!(
                    game_address = %game,
                    tx_hash = ?submission.tx_hash,
                    outcome = ?status.root_state,
                    invalidation_reason = ?status.invalidation_reason,
                    "resolved challenger-owned game"
                );
                resolutions_submitted += 1;
                Ok(())
            }
            .await;

            match result {
                Ok(()) => {}
                Err(error) => {
                    warn!(%error, game_address = %game, "failed to resolve challenger-owned game");
                }
            }
        }

        Ok(())
    }

    /// Runs resolution passes forever on an interval independent of game scanning.
    pub async fn run_forever(&self) -> Result<(), ChallengerError> {
        self.config.validate()?;

        let mut interval = tokio::time::interval(self.config.poll_interval);
        loop {
            interval.tick().await;
            if let Err(error) = self.resolve_games().await {
                warn!(%error, "challenger resolution pass failed; retrying on next tick");
            }
        }
    }
}
