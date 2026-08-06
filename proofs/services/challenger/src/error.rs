use alloy_primitives::{Address, TxHash};
use alloy_provider::{PendingTransactionError, transport::RpcError};
use alloy_transport::TransportErrorKind;
use thiserror::Error;
use world_chain_proof_protocol::{
    ConsensusError, GameStatusError, InvalidationReasonError, ProposalStatusError,
};

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
    #[error(transparent)]
    Contract(Box<alloy_contract::Error>),
    #[error(transparent)]
    OutputRoot(#[from] ConsensusError),
    #[error("The challenge transaction didn't execute succesfully: {0}")]
    Revert(TxHash),
    #[error(transparent)]
    InvalidGameStatus(#[from] GameStatusError),
    #[error(transparent)]
    InvalidProposalStatus(#[from] ProposalStatusError),
    #[error(transparent)]
    NotExistingInvalidReason(#[from] InvalidationReasonError),
    #[error(transparent)]
    PendingTransaction(#[from] PendingTransactionError),
    #[error("Latest L1 block is unavailable.")]
    UnavailableLatestL1Block,
    #[error("Overflow error.")]
    Overflow,
    #[error(transparent)]
    AlloyJsonRpc(#[from] RpcError<TransportErrorKind>),
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

impl From<alloy_contract::Error> for ChallengerError {
    fn from(error: alloy_contract::Error) -> Self {
        Self::Contract(Box::new(error))
    }
}

impl ChallengerError {
    /// Builds an [`AlloyJsonRpc`](Self::AlloyJsonRpc) error from a free-form message.
    ///
    /// Intended for test fakes that need an ad-hoc failure without constructing a full transport
    /// error by hand.
    pub fn message(message: impl AsRef<str>) -> Self {
        Self::AlloyJsonRpc(TransportErrorKind::custom_str(message.as_ref()))
    }
}

/// Error returned while processing a single game.
#[derive(Debug)]
pub(crate) struct GameScanError {
    /// Boxed to keep `Result<_, GameScanError>` small (`clippy::result_large_err`).
    pub error: Box<ChallengerError>,
    pub challenge_deadline: Option<u64>,
}
