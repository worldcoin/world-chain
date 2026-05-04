//! Native account-abstraction node context for the e2e harness.
//!
//! The production node context uses Optimism primitives, where the node-level
//! signed transaction is `OpTransactionSigned`. WIP-1001 needs a node whose
//! primitive transaction type is [`WorldChainTxEnvelope`], otherwise a
//! `0x1d` transaction can only be handled after it has already been rejected by
//! the node's primitive surface.
//!
//! [`Wip1001NodeContext`] is intentionally a normal reth node context rather
//! than a collection of harness aliases. The existing `setup<T>` path can build
//! `WorldChainNode<Wip1001NodeContext>` through
//! `NodeBuilder::with_types_and_provider`, and the associated types below
//! describe the concrete component stack needed for that node.

use alloy_consensus::{Block, BlockBody, Eip658Value, Header, Receipt};
use alloy_evm::{Evm, eth::receipt_builder::ReceiptBuilderCtx};
use alloy_op_evm::block::receipt_builder::OpReceiptBuilder;
use alloy_rpc_types_engine::{ExecutionPayloadEnvelopeV2, ExecutionPayloadV1};
use op_alloy_consensus::OpDepositReceipt;
use op_alloy_rpc_types::{OpTransactionReceipt, OpTransactionRequest};
use op_alloy_rpc_types_engine::{
    OpExecutionData, OpExecutionPayloadEnvelopeV3, OpExecutionPayloadEnvelopeV4,
};
use reth_node_api::{
    BuiltPayload, EngineTypes, FullNodeTypes, NodePrimitives, NodeTypes, PayloadTypes,
};
use reth_node_builder::{
    BuilderContext, NodeAdapter, NodeComponentsBuilder,
    components::{
        BasicPayloadServiceBuilder, ComponentsBuilder, ExecutorBuilder, PayloadServiceBuilder,
    },
    rpc::{BasicEngineValidatorBuilder, RpcAddOns},
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::OpEvmConfig;
use reth_optimism_node::{
    OpAddOns, OpBuiltPayload, OpConsensusBuilder, OpEngineValidatorBuilder, node::OpPayloadBuilder,
    rpc::OpEthApiBuilder, txpool::OpPooledTransaction,
};
use reth_optimism_payload_builder::OpExecData;
use reth_optimism_primitives::OpReceipt;
use reth_primitives_traits::Block as _;
use reth_rpc_api::eth::RpcTypes;
use world_chain_cli::{WorldChainArgs, WorldChainNodeConfig};
use world_chain_node::{
    context::{FlashblocksComponentsContext, WorldChainNetworkBuilder},
    engine::FlashblocksEngineApiBuilder,
    node::{WorldChainNode, WorldChainNodeContext, WorldChainNodePrimitiveTypes},
    pool::WorldChainPoolBuilder,
};
use world_chain_pool::BasicWorldChainPool;
use world_chain_primitives::transaction::{WorldChainTxEnvelope, WorldChainTxType};

/// Node primitives for WIP-1001 e2e tests.
///
/// This is the central type-level change for native account abstraction: blocks
/// still use the standard alloy block/header shape, but their signed
/// transaction type is [`WorldChainTxEnvelope`]. That lets the traditional reth
/// harness launch a node that can decode, validate, pool, execute, and persist
/// WIP-1001 transactions.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Wip1001Primitives;

impl NodePrimitives for Wip1001Primitives {
    type Block = Block<WorldChainTxEnvelope>;
    type BlockHeader = Header;
    type BlockBody = BlockBody<WorldChainTxEnvelope>;
    type SignedTx = WorldChainTxEnvelope;
    type Receipt = OpReceipt;
}

/// Engine payload type set for WIP-1001 nodes.
///
/// This mirrors the OP engine API envelope shapes while removing upstream
/// `OpEngineTypes`' restriction that the built payload primitives must be
/// `OpPrimitives`.
#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct Wip1001EngineTypes;

impl PayloadTypes for Wip1001EngineTypes {
    type ExecutionData = OpExecData;
    type BuiltPayload = OpBuiltPayload<Wip1001Primitives>;
    type PayloadAttributes = reth_optimism_node::payload::OpPayloadAttrs;

    fn block_to_payload(
        block: reth_primitives_traits::SealedBlock<
            <<Self::BuiltPayload as BuiltPayload>::Primitives as NodePrimitives>::Block,
        >,
    ) -> Self::ExecutionData {
        OpExecData::from(OpExecutionData::from_block_unchecked(
            block.hash(),
            &block.into_block().into_ethereum_block(),
        ))
    }
}

impl EngineTypes for Wip1001EngineTypes {
    type ExecutionPayloadEnvelopeV1 = ExecutionPayloadV1;
    type ExecutionPayloadEnvelopeV2 = ExecutionPayloadEnvelopeV2;
    type ExecutionPayloadEnvelopeV3 = OpExecutionPayloadEnvelopeV3;
    type ExecutionPayloadEnvelopeV4 = OpExecutionPayloadEnvelopeV4;
    type ExecutionPayloadEnvelopeV5 = OpExecutionPayloadEnvelopeV4;
    type ExecutionPayloadEnvelopeV6 = OpExecutionPayloadEnvelopeV4;
}

/// Receipt builder for the WIP-1001 e2e primitive set.
///
/// OP receipts do not yet have a dedicated `0x1d` variant. For now the WIP
/// transaction receives an EIP-1559-shaped receipt, matching its fee semantics.
/// The native-AA transaction type remains preserved in the block body.
#[derive(Debug, Default, Clone, Copy)]
pub struct Wip1001ReceiptBuilder;

impl OpReceiptBuilder for Wip1001ReceiptBuilder {
    type Transaction = WorldChainTxEnvelope;
    type Receipt = OpReceipt;

    fn build_receipt<'a, E: Evm>(
        &self,
        ctx: ReceiptBuilderCtx<'a, WorldChainTxType, E>,
    ) -> Result<Self::Receipt, ReceiptBuilderCtx<'a, WorldChainTxType, E>> {
        match ctx.tx_type {
            WorldChainTxType::Deposit => Err(ctx),
            ty => {
                let receipt = Receipt {
                    status: Eip658Value::Eip658(ctx.result.is_success()),
                    cumulative_gas_used: ctx.cumulative_gas_used,
                    logs: ctx.result.into_logs(),
                };

                Ok(match ty {
                    WorldChainTxType::Legacy => OpReceipt::Legacy(receipt),
                    WorldChainTxType::Eip2930 => OpReceipt::Eip2930(receipt),
                    WorldChainTxType::Eip1559 | WorldChainTxType::Wip1001 => {
                        OpReceipt::Eip1559(receipt)
                    }
                    WorldChainTxType::Eip7702 => OpReceipt::Eip7702(receipt),
                    WorldChainTxType::PostExec => OpReceipt::PostExec(receipt),
                    WorldChainTxType::Deposit => unreachable!(),
                })
            }
        }
    }

    fn build_deposit_receipt(&self, inner: OpDepositReceipt) -> Self::Receipt {
        OpReceipt::Deposit(inner)
    }
}

/// Executor builder that ties OP execution to [`Wip1001Primitives`].
#[derive(Debug, Default, Clone, Copy)]
pub struct Wip1001ExecutorBuilder;

impl<Node> ExecutorBuilder<Node> for Wip1001ExecutorBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec = OpChainSpec, Primitives = Wip1001Primitives>>,
{
    type EVM = OpEvmConfig<OpChainSpec, Wip1001Primitives, Wip1001ReceiptBuilder>;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        Ok(OpEvmConfig::new(ctx.chain_spec(), Wip1001ReceiptBuilder))
    }
}

/// RPC type adapter for OP RPC requests over [`WorldChainTxEnvelope`] blocks.
#[derive(Debug, Clone, Copy, Default)]
pub struct Wip1001RpcTypes;

impl RpcTypes for Wip1001RpcTypes {
    type Header = alloy_rpc_types_eth::Header<Header>;
    type Receipt = OpTransactionReceipt;
    type TransactionRequest = OpTransactionRequest;
    type TransactionResponse = op_alloy_rpc_types::Transaction<WorldChainTxEnvelope>;
}

/// World Chain e2e context that launches a WIP-1001-capable node.
///
/// This context keeps the World Chain-specific network builder so flashblocks
/// sub-protocol registration and engine RPC hooks are present when flashblocks
/// are enabled in the test config. The payload builder is the generic OP
/// payload builder because the production flashblocks payload builder is still
/// specialized to `OpPrimitives`; generic flashblocks payload execution can be
/// layered on top of this context separately.
#[derive(Clone, Debug)]
pub struct Wip1001NodeContext {
    config: WorldChainNodeConfig,
    components_context: Option<FlashblocksComponentsContext>,
}

impl Wip1001NodeContext {
    /// Returns the node configuration captured when the context was created.
    pub fn config(&self) -> &WorldChainNodeConfig {
        &self.config
    }
}

impl From<WorldChainNodeConfig> for Wip1001NodeContext {
    fn from(config: WorldChainNodeConfig) -> Self {
        let components_context = config
            .args
            .flashblocks
            .as_ref()
            .map(|_| config.clone().into());

        Self {
            config,
            components_context,
        }
    }
}

impl WorldChainNodePrimitiveTypes for Wip1001NodeContext {
    type Primitives = Wip1001Primitives;
    type Payload = Wip1001EngineTypes;
}

impl<N> WorldChainNodeContext<N> for Wip1001NodeContext
where
    N: FullNodeTypes<Types = WorldChainNode<Self>>,
    BasicPayloadServiceBuilder<OpPayloadBuilder>: PayloadServiceBuilder<
            N,
            BasicWorldChainPool<
                N,
                OpPooledTransaction<WorldChainTxEnvelope, WorldChainTxEnvelope>,
                OpEvmConfig<OpChainSpec, Wip1001Primitives, Wip1001ReceiptBuilder>,
            >,
            OpEvmConfig<OpChainSpec, Wip1001Primitives, Wip1001ReceiptBuilder>,
        >,
{
    type Evm = OpEvmConfig<OpChainSpec, Wip1001Primitives, Wip1001ReceiptBuilder>;
    type Pool = BasicWorldChainPool<
        N,
        OpPooledTransaction<WorldChainTxEnvelope, WorldChainTxEnvelope>,
        Self::Evm,
    >;
    type Net = WorldChainNetworkBuilder;
    type PayloadServiceBuilder = BasicPayloadServiceBuilder<OpPayloadBuilder>;
    type ComponentsBuilder = ComponentsBuilder<
        N,
        WorldChainPoolBuilder<OpPooledTransaction<WorldChainTxEnvelope, WorldChainTxEnvelope>>,
        Self::PayloadServiceBuilder,
        Self::Net,
        Wip1001ExecutorBuilder,
        OpConsensusBuilder,
    >;
    type AddOns = OpAddOns<
        NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>,
        OpEthApiBuilder<Wip1001RpcTypes>,
        OpEngineValidatorBuilder,
        FlashblocksEngineApiBuilder<OpEngineValidatorBuilder>,
        BasicEngineValidatorBuilder<OpEngineValidatorBuilder>,
    >;
    type ExtContext = Option<FlashblocksComponentsContext>;

    fn components(&self) -> Self::ComponentsBuilder {
        let Self {
            config:
                WorldChainNodeConfig {
                    args:
                        WorldChainArgs {
                            rollup,
                            pbh,
                            tx_peers,
                            ..
                        },
                    ..
                },
            components_context,
        } = self.clone();

        let reth_optimism_node::args::RollupArgs {
            disable_txpool_gossip,
            compute_pending_block,
            discovery_v4,
            ..
        } = rollup;

        let wc_network_builder = WorldChainNetworkBuilder::new(
            disable_txpool_gossip,
            !discovery_v4,
            tx_peers,
            components_context
                .as_ref()
                .map(|flashblocks_components_ctx| {
                    flashblocks_components_ctx.flashblocks_handle.clone()
                }),
        );

        ComponentsBuilder::default()
            .node_types::<N>()
            .pool(WorldChainPoolBuilder::new(
                pbh.entrypoint,
                pbh.signature_aggregator,
                pbh.world_id,
            ))
            .executor(Wip1001ExecutorBuilder)
            .payload(BasicPayloadServiceBuilder::new(OpPayloadBuilder::new(
                compute_pending_block,
            )))
            .network(wc_network_builder)
            .consensus(OpConsensusBuilder::default())
    }

    fn add_ons(&self) -> Self::AddOns {
        let engine_api_builder = FlashblocksEngineApiBuilder {
            engine_validator_builder: Default::default(),
            flashblocks_handle: self.components_context.as_ref().map(
                |flashblocks_components_ctx| flashblocks_components_ctx.flashblocks_handle.clone(),
            ),
            to_jobs_generator: self
                .components_context
                .as_ref()
                .map(|flashblocks_components_ctx| {
                    flashblocks_components_ctx.to_jobs_generator.clone()
                }),
            authorizer_vk: self
                .components_context
                .as_ref()
                .map(|flashblocks_components_ctx| flashblocks_components_ctx.authorizer_vk),
        };
        let op_eth_api_builder = OpEthApiBuilder::<Wip1001RpcTypes>::default()
            .with_sequencer(self.config.args.rollup.sequencer.clone());

        let engine_validator_builder =
            BasicEngineValidatorBuilder::<OpEngineValidatorBuilder>::default();

        let rpc_add_ons = RpcAddOns::new(
            op_eth_api_builder,
            Default::default(),
            engine_api_builder,
            engine_validator_builder,
            Default::default(),
        );

        OpAddOns::new(
            rpc_add_ons,
            self.config.builder_config.inner.da_config.clone(),
            self.config.builder_config.inner.gas_limit_config.clone(),
            self.config.args.rollup.sequencer.clone(),
            Default::default(),
            Default::default(),
            false,
            1_000_000,
        )
    }

    fn ext_context(&self) -> Self::ExtContext {
        self.components_context.clone()
    }
}
