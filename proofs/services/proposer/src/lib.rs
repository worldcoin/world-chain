//! World Chain proposer primitives.
//!
//! The proposer watches L2 output roots and creates WIP-1006 `MultiProofGame`
//! contracts on L1 through the stock OP Stack `DisputeGameFactory`.

mod alloy;
mod bond_manager;
mod config;
mod error;
pub mod metrics;
mod proposer;
mod traits;
mod types;

// re-exports
pub use alloy::AlloyProofSystemClient;
pub use bond_manager::BondManager;
pub use config::{
    BondManagerConfig, DEFAULT_BOND_MANAGER_INITIAL_SCAN_LIMIT, DEFAULT_BOND_MANAGER_POLL_INTERVAL,
    ProposerConfig,
};
pub use error::ProposerError;
pub use proposer::WorldChainProposer;
pub use traits::{BondManagerClient, ProposerClient};
pub use types::{
    CloseGameSubmission, NextProposalAction, Proposal, ProposalSubmission, ProposerScan,
    ResolveSubmission,
};

#[cfg(test)]
mod tests;
