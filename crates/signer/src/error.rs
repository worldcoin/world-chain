use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

/// Errors exclude request bodies, credentials and response-provided free-form text.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Error {
    #[error("invalid local PBH private key")]
    InvalidLocalKey,
    #[error("local PBH transaction signing failed")]
    LocalSigning,
    #[error("invalid remote signer configuration: {0}")]
    Config(&'static str),
    #[error("remote signer {operation} failed: {detail}")]
    Transport {
        operation: &'static str,
        detail: String,
    },
    #[error("remote signer returned HTTP {0}")]
    Http(u16),
    #[error("remote signer request deadline exceeded")]
    Deadline,
    #[error("remote signer request cancelled")]
    Cancelled,
    #[error("remote signer client is busy; no request was queued")]
    Busy,
    #[error("remote signer {0} exceeds the V1 size bound")]
    TooLarge(&'static str),
    #[error("invalid remote signer response: {0}")]
    InvalidResponse(&'static str),
    #[error("remote signer signature does not match the configured identity and request")]
    InvalidSignature,
    #[error("remote signer denied request: {reason} ({code})")]
    Remote { code: i64, reason: &'static str },
}

impl Error {
    pub(crate) fn transport(operation: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Transport {
            operation,
            detail: error.to_string(),
        }
    }
}
