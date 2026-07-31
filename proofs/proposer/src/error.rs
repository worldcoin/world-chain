use alloy_primitives::TxHash;
use thiserror::Error;
use world_chain_proofs::LineageError;

/// Errors returned by the proposer.
#[derive(Debug, Error)]
pub enum ProposerError {
    /// Invalid proposer configuration.
    #[error("invalid proposer config: {0}")]
    InvalidConfig(&'static str),
    /// Contract call or transaction failure.
    #[error("contract error: {0}")]
    Contract(String),
    #[error(transparent)]
    Lineage(#[from] LineageError),
    #[error("The proposal transaction didn't execute succesfully: {0}")]
    Revert(TxHash),
}
