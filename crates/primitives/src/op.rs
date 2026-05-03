//! OP-stack symbol re-exports used by world-chain.
//!
//! Single point where world-chain imports anything from the op-reth crates in
//! the optimism monorepo. Every other world-chain crate goes through
//! `world_chain_primitives::*` so the op-reth surface is confined to one place.

pub use alloy_op_hardforks::{OpHardfork, OpHardforks};
pub use op_alloy_consensus::OpReceipt;
pub use reth_optimism_chainspec::{OpChainSpec, OpChainSpecBuilder, BASE_MAINNET};
pub use reth_optimism_cli::{chainspec::OpChainSpecParser, Cli};
pub use reth_optimism_evm::{
    extract_l1_info, OpBlockAssembler, OpEvmConfig, OpNextBlockEnvAttributes, OpRethReceiptBuilder,
};
pub use reth_optimism_node::{
    args::RollupArgs,
    node::{
        OpAddOns, OpConsensusBuilder, OpEngineValidatorBuilder, OpExecutorBuilder, OpNetworkBuilder,
    },
    payload::{OpExecData, OpPayloadAttributes},
    txpool::{
        conditional::MaybeConditionalTransaction, estimated_da_size::DataAvailabilitySized,
        interop::MaybeInteropTransaction, OpPooledTransaction, OpPooledTx, OpTransactionValidator,
    },
    OpEngineTypes, OpNodeTypes, OpStorage, OP_NAME_CLIENT,
};
pub use reth_optimism_payload_builder::{
    builder::{ExecutionInfo, OpPayloadBuilderCtx, OpPayloadTransactions},
    config::{OpBuilderConfig, OpGasLimitConfig},
    payload::{OpBuiltPayload, OpPayloadAttrs, OpPayloadBuilderAttributes},
};
pub use reth_optimism_primitives::OpPrimitives;
pub use reth_optimism_rpc::{
    eth::OpRpcConvert, OpEngineApi, OpEngineApiServer, OpEthApi, OpEthApiBuilder, OpEthApiError,
    OpReceiptBuilder, OP_ENGINE_CAPABILITIES,
};

/// Signed OP-stack transaction (alias of [`op_alloy_consensus::OpTxEnvelope`]).
pub type OpTransactionSigned = op_alloy_consensus::OpTxEnvelope;

/// Optimism-specific block type.
pub type OpBlock = alloy_consensus::Block<OpTransactionSigned>;

/// Optimism-specific block body type.
pub type OpBlockBody = <OpBlock as reth_primitives_traits::Block>::Body;

/// Helper function to create new OP-stack payload attributes.
pub const fn optimism_payload_attributes(timestamp: u64) -> OpPayloadAttrs {
    OpPayloadAttrs(OpPayloadAttributes {
        payload_attributes: alloy_rpc_types_engine::PayloadAttributes {
            timestamp,
            prev_randao: alloy_primitives::B256::ZERO,
            suggested_fee_recipient: alloy_primitives::Address::ZERO,
            withdrawals: Some(vec![]),
            parent_beacon_block_root: Some(alloy_primitives::B256::ZERO),
        },
        transactions: None,
        no_tx_pool: None,
        gas_limit: Some(30_000_000),
        eip_1559_params: None,
        min_base_fee: None,
    })
}
