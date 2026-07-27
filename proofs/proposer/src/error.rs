use alloy_primitives::TxHash;
use thiserror::Error;
use world_chain_proofs::ConsensusError;
use world_chain_prover_service::ProofRequestError;

/// Errors returned by the proposer.
#[derive(Debug, Error)]
pub enum ProposerError {
    /// Invalid proposer configuration.
    #[error("invalid proposer config: {0}")]
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
    #[error("L1 finalized block not found")]
    FinalizedBlockNotFound,
    /// Prover-service request failure.
    #[error(transparent)]
    ProofRequest(#[from] ProofRequestError),
    /// Prover-service returned data inconsistent with the requested proof.
    #[error("invalid proof response: {0}")]
    InvalidProofResponse(String),
    #[error(transparent)]
    OutputRoot(#[from] ConsensusError),
    #[error("The proposal transaction didn't execute succesfully: {0}")]
    Revert(TxHash),
}
