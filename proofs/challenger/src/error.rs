use thiserror::Error;
use world_chain_proofs::OutputRootError;

/// Errors returned by the proposer.
#[derive(Debug, Error)]
pub enum ChallengerError {
    /// Invalid challenger configuration.
    #[error("invalid challenger config: {0}")]
    InvalidConfig(&'static str),
    /// Adding `block_interval` overflowed `u64`.
    #[error("l2 block number overflow: parent {parent_block} + interval {block_interval}")]
    BlockNumberOverflow {
        /// Parent L2 block number.
        parent_block: u64,
        /// Configured block interval.
        block_interval: u64,
    },
    /// Contract call or transaction failure.
    #[error("contract error: {0}")]
    Contract(String),
    #[error(transparent)]
    OutputRoot(#[from] OutputRootError),
    #[error("The challenge transaction didn't execute succesfully")]
    Revert,
    #[error("Invalid root state: {0}")]
    InvalidRootState(u8),
}
