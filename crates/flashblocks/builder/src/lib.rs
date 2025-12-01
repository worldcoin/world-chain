#![warn(unused_crate_dependencies)]
#![allow(clippy::type_complexity)]

use reth_optimism_payload_builder::config::OpBuilderConfig;

pub mod access_list;
pub mod assembler;
pub mod block_builder;
pub mod coordinator;
pub mod executor;
pub mod payload_builder;
pub mod payload_txns;
pub mod traits;

#[derive(Debug, Clone)]
pub struct FlashblocksPayloadBuilderConfig {
    /// Inner OP Payload Builder Configuration
    pub inner: OpBuilderConfig,
    /// Whether to enable BAL support
    pub bal_enabled: bool,
}

impl Default for FlashblocksPayloadBuilderConfig {
    fn default() -> Self {
        Self {
            inner: OpBuilderConfig::default(),
            bal_enabled: false,
        }
    }
}
