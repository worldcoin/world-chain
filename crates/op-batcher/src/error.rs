//! Unified error type for the OP Batcher ExEx.
//!
//! Mirrors the structure of the proposer crate's `OpProposerError`: a single
//! top-level error chaining every per-domain error, with `msg`/`from_boxed`
//! escape hatches backed by `eyre`.

use std::error::Error as StdError;

use eyre::eyre::eyre;
use thiserror::Error;

use crate::{
    config::BatcherConfigError, db::BatcherStoreError, provider::ProviderError, rpc::AdminRpcError,
    source::BlockSourceError,
};

/// Top-level error for the OP Batcher ExEx.
#[derive(Debug, Error)]
pub enum OpBatcherError {
    /// L1 provider construction failed.
    #[error(transparent)]
    Provider(#[from] ProviderError),

    /// Local block source (reth node state) failed.
    #[error(transparent)]
    Source(#[from] BlockSourceError),

    /// MDBX-backed batcher store failed.
    #[error(transparent)]
    Store(#[from] BatcherStoreError),

    /// Admin RPC server failed (bind / start).
    #[error(transparent)]
    Rpc(#[from] AdminRpcError),

    /// CLI -> runtime config translation failed.
    #[error(transparent)]
    Config(#[from] BatcherConfigError),

    /// Failed to await a pending L1 transaction's receipt.
    #[error(transparent)]
    PendingTransaction(#[from] alloy_provider::PendingTransactionError),

    /// JSON-RPC transport failure.
    #[error(transparent)]
    Transport(#[from] alloy_transport::TransportError),

    /// Channel/frame/batch encoding failed.
    #[error("channel encoding error: {0}")]
    Channel(String),

    /// Driver state error: start requested while already running.
    #[error("batcher is already running")]
    AlreadyRunning,

    /// Driver state error: stop requested while not running.
    #[error("batcher is not running")]
    NotRunning,

    /// Ad-hoc error with a captured backtrace (via `eyre`).
    #[error(transparent)]
    Eyre(#[from] eyre::eyre::Report),

    /// Catch-all for foreign trait objects.
    #[error(transparent)]
    Boxed(#[from] Box<dyn StdError + Send + Sync + 'static>),
}

impl OpBatcherError {
    /// Construct an [`OpBatcherError::Eyre`] from a context string, capturing a
    /// backtrace at the call site.
    pub fn msg(msg: impl Into<String>) -> Self {
        Self::Eyre(eyre!(msg.into()))
    }

    /// Wrap any `std::error::Error` in [`OpBatcherError::Boxed`].
    pub fn from_boxed<E: StdError + Send + Sync + 'static>(err: E) -> Self {
        Self::Boxed(Box::new(err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ensure_send_sync<T: Send + Sync + 'static>() {}

    #[test]
    fn op_batcher_error_is_send_sync_static() {
        ensure_send_sync::<OpBatcherError>();
    }

    #[test]
    fn msg_constructor_renders_message() {
        let err = OpBatcherError::msg("custom message");
        assert!(format!("{err}").contains("custom message"));
    }
}
