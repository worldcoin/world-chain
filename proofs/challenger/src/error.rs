use alloy_primitives::{Address, TxHash};
use thiserror::Error;
use world_chain_proofs::{ConsensusError, RootStateError};

/// Errors returned by the challenger and its lifecycle managers.
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
    #[error(transparent)]
    InvalidRootState(#[from] RootStateError),
    #[error("RPC error: {0}")]
    Rpc(String),
    #[error("Latest L1 finalized block not found")]
    L1FinalizedBlockNotFound,
    #[error(
        "L2 block included in the game {game} is not finalized yet. latest_finalized: {latest_finalized}, given_block: {given_block}"
    )]
    L2BlockNotFinalized {
        /// Address of the game.
        game: Address,
        /// Latest L2 finalized block number.
        latest_finalized: u64,
        /// Block number included in the game.
        given_block: u64,
    },
}

/// Error returned while processing a single game.
#[derive(Debug)]
pub(crate) struct GameScanError {
    pub error: ChallengerError,
    pub challenge_deadline: Option<u64>,
}
