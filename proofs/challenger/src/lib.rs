//! World Chain Challenger.

mod alloy;
mod challenger;
mod config;
mod error;
mod traits;
mod types;

// re-exports
pub use alloy::AlloyChallengerClient;
pub use challenger::WorldChainChallenger;
pub use config::ChallengerConfig;
pub use error::ChallengerError;
pub use traits::ChallengerClient;
pub use types::ChallengeSubmission;

#[cfg(test)]
mod tests;
