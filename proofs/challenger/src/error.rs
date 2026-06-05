use alloy_primitives::TxHash;
use thiserror::Error;
use world_chain_proofs::ConsensusError;

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
    OutputRoot(#[from] ConsensusError),
    #[error("The challenge transaction didn't execute succesfully: {0}")]
    Revert(TxHash),
    #[error("Invalid root state: {0}")]
    InvalidRootState(u8),
    #[error("RPC error: {0}")]
    Rpc(String),
    #[error("Latest L1 finalized block not found")]
    L1FinalizedBlockNotFound(),
}
