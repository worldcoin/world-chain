use thiserror::Error;

#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub enum FlashblocksP2PError {
    #[error(transparent)]
    Signing(#[from] world_chain_signer::Error),
    #[error("complete flashblock frame exceeds 16 MiB")]
    FrameTooLarge,
    #[error("remote publication requires its live publishing lease")]
    MissingPublishingLease,
    #[error("attempt to publish flashblocks without clearance")]
    NotClearedToPublish,
    #[error(
        "attempt to publish flashblocks with expired authorization. Make sure to call `start_publishing` first"
    )]
    ExpiredAuthorization,
    #[error("builder signing key has not been configured")]
    MissingBuilderSk,
    #[error("publishing control request was superseded or its payload changed")]
    StaleControlMessage,
    #[error("invalid signature or fields in publishing control message")]
    InvalidControlMessage,
}
