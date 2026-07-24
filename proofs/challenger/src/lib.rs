//! World Chain Challenger.

mod alloy;
mod bond_manager;
mod challenger;
mod config;
mod error;
mod resolution_manager;
mod traits;
mod types;

// re-exports
pub use alloy::AlloyChallengerClient;
pub use bond_manager::BondManager;
pub use challenger::WorldChainChallenger;
pub use config::{
    BondManagerConfig, ChallengerConfig, DEFAULT_BOND_MANAGER_INITIAL_SCAN_LIMIT,
    DEFAULT_BOND_MANAGER_POLL_INTERVAL, DEFAULT_MAX_GAME_CONCURRENCY, DEFAULT_MAX_GAMES_PER_TICK,
    DEFAULT_MAX_RESOLUTIONS_PER_TICK, DEFAULT_RESOLUTION_POLL_INTERVAL, ResolutionManagerConfig,
};
pub use error::ChallengerError;
pub use resolution_manager::ResolutionManager;
pub use traits::{BondManagerClient, ChallengerClient, ResolutionManagerClient};
pub use types::{
    ChallengeSubmission, GameMetadata, OwnedGames, ResolveSubmission, WithdrawSubmission,
};

#[cfg(test)]
mod tests;
