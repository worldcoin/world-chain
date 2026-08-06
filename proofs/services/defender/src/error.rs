use alloy_primitives::{Address, TxHash};
use alloy_provider::{MulticallError, PendingTransactionError, transport::RpcError};
use alloy_transport::TransportErrorKind;
use thiserror::Error;
use world_chain_proof_protocol::{InvalidationReasonError, LineageError, ProofLane, ProposalStatusError};

/// Errors returned by the defender.
#[derive(Debug, Error)]
pub enum DefenderError {
    /// Invalid defender configuration.
    #[error("invalid defender config: {0}")]
    InvalidConfig(&'static str),
    /// Contract call or transaction failure.
    #[error(transparent)]
    Contract(#[from] alloy_contract::Error),
    /// A transaction failed while waiting for its receipt.
    #[error(transparent)]
    PendingTransaction(#[from] PendingTransactionError),
    /// A multicall request failed.
    #[error(transparent)]
    Multicall(#[from] MulticallError),
    /// An Alloy JSON-RPC request failed.
    #[error(transparent)]
    AlloyJsonRpc(#[from] RpcError<TransportErrorKind>),
    /// Prover response could not be encoded for its on-chain verifier.
    #[error("invalid proof payload: {0}")]
    ProofEncoding(String),
    #[error(transparent)]
    Lineage(#[from] LineageError),
    #[error(transparent)]
    InvalidProposalStatus(#[from] ProposalStatusError),
    #[error(transparent)]
    InvalidInvalidationReason(#[from] InvalidationReasonError),
    #[error("The submitProofLane transaction didn't execute succesfully: {0}")]
    Revert(TxHash),
    /// The game rejected the submission because the lane already counts toward its threshold.
    #[error("lane {lane:?} already proven for game {game}")]
    LaneAlreadyProven { game: Address, lane: ProofLane },
    #[error("Overflow error.")]
    Overflow,
    #[error("Invalid proof threshold {proof_threshold} for game {game}")]
    InvalidProofThreshold { proof_threshold: u8, game: Address },
}

impl DefenderError {
    /// Builds an [`AlloyJsonRpc`](Self::AlloyJsonRpc) error from a free-form message.
    ///
    /// Intended for test fakes that need an ad-hoc failure without constructing a full transport
    /// error by hand.
    pub fn message(message: impl AsRef<str>) -> Self {
        Self::AlloyJsonRpc(TransportErrorKind::custom_str(message.as_ref()))
    }
}
