use alloy_primitives::TxHash;
use thiserror::Error;
use world_chain_proofs::OutputRootError;

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
    #[error(transparent)]
    OutputRoot(#[from] OutputRootError),
    #[error("The proposal transaction didn't execute succesfully: {0}")]
    Revert(TxHash),
    /// The next proposal target is ahead of the op-node L2 head, so no output
    /// root is available yet. Transient and expected while the L2 catches up.
    #[error("next proposal target {target_block} is ahead of the L2 head {l2_head}")]
    ProposalNotReady {
        /// L2 block number the proposer wants to propose next.
        target_block: u64,
        /// Current L2 head reported by the op-node.
        l2_head: u64,
    },
}
