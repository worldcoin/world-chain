//! World Chain proposer primitives.
//!
//! The proposer watches L2 output roots and creates `WorldChainProofSystemGame`
//! contracts on L1 through `WorldChainProofSystemFactory`.

mod alloy;
mod config;
mod error;
mod proposer;
mod traits;
mod types;

// re-exports
pub use alloy::AlloyProofSystemClient;
pub use config::ProposerConfig;
pub use error::ProposerError;
pub use proposer::WorldChainProposer;
pub use traits::ProposerClient;
pub use types::{
    CanonicalLine, CanonicalScan, CloseGameSubmission, FinalizedGames, NextProposalAction,
    ParentRef, Proposal, ProposalSubmission, ResolveSubmission, WithdrawSubmission,
};

#[cfg(test)]
mod tests;
