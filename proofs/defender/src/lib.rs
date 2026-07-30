//! World Chain Defender.

mod alloy;
mod config;
mod defender;
mod error;
mod game;
mod lane;
mod traits;
mod types;

// re-exports
pub use alloy::AlloyDefenderClient;
pub use config::{DEFAULT_GAME_SCAN_LOOKBACK, DEFAULT_L1_TX_CONFIRMATIONS, DefenderConfig};
pub use defender::WorldChainDefender;
pub use error::DefenderError;
pub use traits::DefenderClient;
pub use types::{DefenderSubmission, GameMetadata};

#[cfg(test)]
mod tests;
