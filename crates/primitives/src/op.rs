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
    utils::optimism_payload_attributes,
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
