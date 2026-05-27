//! Unified error type for the OP Proposer ExEx.
//!
//! [`OpProposerError`] is the single top-level error type returned from
//! `pub` functions in this crate. It chains every per-domain error
//! ([`ProviderError`], [`ContractError`], [`ProposalSourceError`],
//! [`ProposerStoreError`], [`AdminRpcError`], [`ProposerConfigError`]) and
//! the leaf-level errors raised by alloy (contract submission, pending
//! transaction polling, JSON-RPC transport).
//!
//! For ad-hoc errors / context that doesn't fit a specific variant, use:
//!
//! * [`OpProposerError::Eyre`] — wraps an [`eyre::Report`]. `eyre` captures
//!   a backtrace at construction (subject to `RUST_BACKTRACE` /
//!   `RUST_LIB_BACKTRACE`).
//! * [`OpProposerError::Boxed`] — wraps an arbitrary `Box<dyn std::error::Error
//!   + Send + Sync>`, useful for FFI / interop boundaries that already hand us
//!   trait objects.
//!
//! Each variant uses `#[error(transparent)]` so the upstream error's
//! `Display` and `source()` chain are surfaced unchanged. Combined with
//! eyre's backtrace handling, callers can pretty-print
//! `OpProposerError` with `tracing::error!(error = ?e)` and get the full
//! cause chain plus a backtrace for the originating site.

use std::error::Error as StdError;

use eyre::eyre::eyre;
use thiserror::Error;

use crate::{
    config::ProposerConfigError, contracts::ContractError, db::ProposerStoreError,
    provider::ProviderError, rpc::AdminRpcError, source::ProposalSourceError,
};

/// Top-level error for the OP Proposer ExEx.
#[derive(Debug, Error)]
pub enum OpProposerError {
    /// L1 provider construction failed.
    #[error(transparent)]
    Provider(#[from] ProviderError),

    /// DGF contract read failed.
    #[error(transparent)]
    Contract(#[from] ContractError),

    /// Proposal source (local node / rollup RPC) failed.
    #[error(transparent)]
    Source(#[from] ProposalSourceError),

    /// MDBX-backed proposer store failed.
    #[error(transparent)]
    Store(#[from] ProposerStoreError),

    /// Admin RPC server failed (bind / start).
    #[error(transparent)]
    Rpc(#[from] AdminRpcError),

    /// CLI -> runtime config translation failed.
    #[error(transparent)]
    Config(#[from] ProposerConfigError),

    /// Failed to submit / build a contract call (from
    /// [`alloy_contract::CallBuilder::send`]).
    #[error(transparent)]
    Submit(#[from] alloy_contract::Error),

    /// Failed to await a pending L1 transaction's receipt.
    #[error(transparent)]
    PendingTransaction(#[from] alloy_provider::PendingTransactionError),

    /// JSON-RPC transport failure outside of a contract call.
    #[error(transparent)]
    Transport(#[from] alloy_transport::TransportError),

    /// Driver state error: the caller asked us to start a proposer that's
    /// already running.
    #[error("proposer is already running")]
    AlreadyRunning,

    /// Driver state error: the caller asked us to stop a proposer that was
    /// never started.
    #[error("proposer is not running")]
    NotRunning,

    /// Ad-hoc error with a captured backtrace (via `eyre`).
    #[error(transparent)]
    Eyre(#[from] eyre::eyre::Report),

    /// Catch-all for foreign trait objects (e.g. crossing FFI boundaries).
    #[error(transparent)]
    Boxed(#[from] Box<dyn StdError + Send + Sync + 'static>),
}

impl OpProposerError {
    /// Construct an [`OpProposerError::Eyre`] from a context string. The
    /// resulting variant captures a backtrace at the call site.
    pub fn msg(msg: impl Into<String>) -> Self {
        Self::Eyre(eyre!(msg.into()))
    }

    /// Wrap any `std::error::Error` in [`OpProposerError::Boxed`]. Useful
    /// when interfacing with code that returns `Box<dyn Error>`.
    pub fn from_boxed<E: StdError + Send + Sync + 'static>(err: E) -> Self {
        Self::Boxed(Box::new(err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ensure_send_sync<T: Send + Sync + 'static>() {}

    #[test]
    fn op_proposer_error_is_send_sync_static() {
        ensure_send_sync::<OpProposerError>();
    }

    #[test]
    fn eyre_variant_preserves_source_chain() {
        let inner = std::io::Error::new(std::io::ErrorKind::Other, "boom");
        let report: eyre::eyre::Report = eyre!(inner).wrap_err("outer context");
        let err: OpProposerError = report.into();
        let s = format!("{err}");
        assert!(s.contains("outer context"));
    }

    #[test]
    fn msg_constructor_renders_message() {
        let err = OpProposerError::msg("custom message");
        assert!(format!("{err}").contains("custom message"));
    }
}
