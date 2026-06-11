//! World Chain Defender.

mod alloy;
mod config;
mod defender;
mod error;
mod traits;
mod types;

// re-exports
pub use alloy::AlloyDefenderClient;
pub use config::DefenderConfig;
pub use defender::WorldChainDefender;
pub use error::DefenderError;
pub use traits::DefenderClient;
pub use types::DefenderSubmission;

#[cfg(test)]
mod tests;
