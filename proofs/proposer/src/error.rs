use thiserror::Error;

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
    /// RPC transport or JSON-RPC failure.
    #[error("rpc error: {0}")]
    Rpc(String),
    /// The output-root RPC response did not contain an output root.
    #[error("optimism_outputAtBlock response did not contain an output root")]
    MissingOutputRoot,
    #[error("The proposal transaction didn't execute succesfully")]
    Revert,
}
