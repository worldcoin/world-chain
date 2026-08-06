use alloy_primitives::TxHash;
use alloy_provider::{MulticallError, PendingTransactionError, transport::RpcError};
use alloy_transport::TransportErrorKind;
use thiserror::Error;
use world_chain_proofs::LineageError;

/// Errors returned by the proposer.
#[derive(Debug, Error)]
pub enum ProposerError {
    /// Invalid proposer configuration.
    #[error("invalid proposer config: {0}")]
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
    #[error(transparent)]
    Lineage(#[from] LineageError),
    #[error("The proposal transaction didn't execute succesfully: {0}")]
    Revert(TxHash),
    #[error("Overflow error.")]
    Overflow,
    #[error("Latest L1 block is unavailable.")]
    UnavailableLatestL1Block,
    #[error("DisputeGameCreated event missing from proposal transaction {0}")]
    MissingProposalEvent(TxHash),
}

impl ProposerError {
    /// Builds an [`AlloyJsonRpc`](Self::AlloyJsonRpc) error from a free-form message.
    ///
    /// Intended for test fakes that need an ad-hoc failure without constructing a full transport
    /// error by hand.
    pub fn message(message: impl AsRef<str>) -> Self {
        Self::AlloyJsonRpc(TransportErrorKind::custom_str(message.as_ref()))
    }
}
