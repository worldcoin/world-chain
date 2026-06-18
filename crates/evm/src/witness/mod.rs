//! Witness-capturing wrappers over [`WorldChainEvmConfig`](crate::WorldChainEvmConfig).
//!
//! These types wrap the node's EVM configuration so that, when active, each block's
//! [`ExecutionWitnessRecord`](reth_revm::witness::ExecutionWitnessRecord) is captured directly from
//! the live execution state cache during block execution, with zero re-execution.
//!
//! The capture is opt-in: when constructed without a sender the wrappers are pure passthroughs over
//! the inner config, and the EVM behaves identically to the unwrapped configuration.

mod config;
mod executor;
mod factory;

pub use config::{CapturedBlock, WitnessCapturingEvmConfig};
pub use executor::WitnessExecutor;
pub use factory::WitnessBlockExecutorFactory;
