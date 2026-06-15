use alloy_primitives::TxHash;
use thiserror::Error;
use world_chain_proofs::{ConsensusError, RootStateError};

/// Errors returned by the defender.
#[derive(Debug, Error)]
pub enum DefenderError {
    /// Invalid defender configuration.
    #[error("invalid defender config: {0}")]
    InvalidConfig(&'static str),
    /// Contract call or transaction failure.
    #[error("contract error: {0}")]
    Contract(String),
    #[error("RPC error: {0}")]
    Rpc(String),
    #[error("Latest L1 finalized block not found")]
    L1FinalizedBlockNotFound,
    #[error(transparent)]
    InvalidRootState(#[from] RootStateError),
    #[error(transparent)]
    OutputRoot(#[from] ConsensusError),
    #[error("The submitProofLane transaction didn't execute succesfully: {0}")]
    Revert(TxHash),
}
