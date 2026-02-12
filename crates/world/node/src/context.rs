// Module defining World Chain Node Preset contexts for components & add-ons.

use std::time::Duration;

use crate::{
    args::WorldChainArgs,
    config::WorldChainNodeConfig,
    node::{
        WorldChainNode, WorldChainNodeComponentBuilder, WorldChainNodeContext,
        WorldChainPoolBuilder,
    },
};
use ed25519_dalek::VerifyingKey;
use flashblocks_builder::coordinator::FlashblocksExecutionCoordinator;
use flashblocks_node::{
    engine::FlashblocksEngineApiBuilder, payload::FlashblocksPayloadBuilderBuilder,
    payload_service::FlashblocksPayloadServiceBuilder,
};
use flashblocks_p2p::{net::FlashblocksNetworkBuilder, protocol::handler::FlashblocksHandle};
use flashblocks_primitives::p2p::Authorization;
use flashblocks_rpc::eth::FlashblocksEthApiBuilder;
use reth_node_api::{FullNodeTypes, NodeTypes};
use reth_node_builder::{
    NodeAdapter, NodeComponentsBuilder,
    components::{ComponentsBuilder, PayloadServiceBuilder},
    rpc::{BasicEngineValidatorBuilder, RpcAddOns},
};
use reth_optimism_evm::OpEvmConfig;
use reth_optimism_node::{
    OpAddOns, OpConsensusBuilder, OpEngineValidatorBuilder, OpExecutorBuilder, OpNetworkBuilder,
    args::RollupArgs,
};
use reth_optimism_rpc::OpEthApiBuilder;

use world_chain_payload::context::WorldChainPayloadBuilderCtxBuilder;
use world_chain_pool::BasicWorldChainPool;

use crate::tx_propagation::WorldChainTransactionPropagationPolicy;
use reth::primitives::Hardforks;
use reth_network::PeersInfo;
use reth_network_peers::PeerId;
use reth_node_builder::{BuilderContext, components::NetworkBuilder};
use reth_transaction_pool::{PoolTransaction, TransactionPool};

/// Network builder for World Chain that optionally applies custom transaction propagation policy.
///
/// Extends OpNetworkBuilder to support restricting transaction gossip to specific peers.
#[derive(Debug, Clone)]
pub struct WorldChainNetworkBuilder {
    op_network_builder: OpNetworkBuilder,
    tx_peers: Option<Vec<PeerId>>,
}

impl WorldChainNetworkBuilder {
    pub fn new(
        disable_txpool_gossip: bool,
        disable_discovery_v4: bool,
        tx_peers: Option<Vec<PeerId>>,
    ) -> Self {
        let op_network_builder = OpNetworkBuilder {
            disable_txpool_gossip,
            disable_discovery_v4,
        };

        Self {
            op_network_builder,
            tx_peers,
        }
    }
}

impl<Node, Pool> NetworkBuilder<Node, Pool> for WorldChainNetworkBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec: Hardforks>>,
    Pool: TransactionPool<
            Transaction: PoolTransaction<
                Consensus = <<Node::Types as NodeTypes>::Primitives as reth_node_api::NodePrimitives>::SignedTx,
            >,
        > + Unpin
        + 'static,
{
    type Network = <OpNetworkBuilder as NetworkBuilder<Node, Pool>>::Network;

    async fn build_network(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<Self::Network> {
        let network_config = self.op_network_builder.network_config(ctx)?;

        let network = reth_network::NetworkManager::builder(network_config).await?;

        // Start network with custom policy if specified, otherwise use default
        let handle = if let Some(peers) = self.tx_peers {
            tracing::info!(
                target: "world_chain::network",
                "Applying peer white listing transaction policy. Number of peers: {}",
                peers.len()
            );
            let policy = WorldChainTransactionPropagationPolicy::new(peers);
            let tx_config = ctx.config().network.transactions_manager_config();
            ctx.start_network_with(network, pool, tx_config, policy)
        } else {
            tracing::info!(
                target: "world_chain::network",
                "Starting network with default propagation policy"
            );
            ctx.start_network(network, pool)
        };

        tracing::info!(
            target: "world_chain::network",
            enode = %handle.local_node_record(),
            "World Chain P2P networking initialized"
        );

        Ok(handle)
    }
}

#[derive(Clone, Debug)]
pub struct FlashblocksContext {
    config: WorldChainNodeConfig,
    components_context: Option<FlashblocksComponentsContext>,
}

impl<N: FullNodeTypes<Types = WorldChainNode<FlashblocksContext>>> WorldChainNodeContext<N>
    for FlashblocksContext
where
    FlashblocksPayloadServiceBuilder<
        FlashblocksPayloadBuilderBuilder<WorldChainPayloadBuilderCtxBuilder>,
    >: PayloadServiceBuilder<
            N,
            BasicWorldChainPool<N>,
            OpEvmConfig<<<N as FullNodeTypes>::Types as NodeTypes>::ChainSpec>,
        >,
{
    type Net = FlashblocksNetworkBuilder<WorldChainNetworkBuilder>;
    type Evm = OpEvmConfig;
    type PayloadServiceBuilder = FlashblocksPayloadServiceBuilder<
        FlashblocksPayloadBuilderBuilder<WorldChainPayloadBuilderCtxBuilder>,
    >;

    type ComponentsBuilder = WorldChainNodeComponentBuilder<N, Self>;

    type AddOns = OpAddOns<
        NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>,
        FlashblocksEthApiBuilder,
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
                            builder,
                            pbh,
                            tx_peers,
                            ..
                        },
                    builder_config,
                },
            components_context,
        } = self.clone();

        let RollupArgs {
            disable_txpool_gossip,
            compute_pending_block: _,
            discovery_v4,
            ..
        } = rollup;

        let wc_network_builder =
            WorldChainNetworkBuilder::new(disable_txpool_gossip, !discovery_v4, tx_peers);

        let (flashblocks_interval, flashblocks_recommit_interval, spoof_authorizer_sk) =
            if let Some(flashblocks_args) = self.config.args.flashblocks.as_ref() {
                (
                    flashblocks_args.flashblocks_interval,
                    flashblocks_args.recommit_interval,
                    flashblocks_args.spoof_authorizer_sk.clone(),
                )
            } else {
                // Not important if flashblocks is not enabled. Put some numbers just to make
                // the compiler work fine.
                (200, 200, None)
            };

        let fb_network_builder = FlashblocksNetworkBuilder::new(
            wc_network_builder,
            components_context
                .as_ref()
                .map(|flahsblocks_components_ctx| {
                    flahsblocks_components_ctx.flashblocks_handle.clone()
                }),
        );

        let ctx_builder = WorldChainPayloadBuilderCtxBuilder {
            verified_blockspace_capacity: pbh.verified_blockspace_capacity,
            pbh_entry_point: pbh.entrypoint,
            pbh_signature_aggregator: pbh.signature_aggregator,
            builder_private_key: builder.private_key,
        };

        ComponentsBuilder::default()
            .node_types::<N>()
            .pool(WorldChainPoolBuilder::new(
                pbh.entrypoint,
                pbh.signature_aggregator,
                pbh.world_id,
            ))
            .executor(OpExecutorBuilder::default())
            .payload(FlashblocksPayloadServiceBuilder::new(
                FlashblocksPayloadBuilderBuilder::new(
                    ctx_builder,
                    components_context
                        .as_ref()
                        .map(|flashblocks_component_ctx| {
                            flashblocks_component_ctx.flashblocks_state.clone()
                        }),
                    builder_config,
                ),
                components_context
                    .as_ref()
                    .map(|flashblocks_components_ctx| {
                        flashblocks_components_ctx.flashblocks_handle.clone()
                    }),
                components_context
                    .as_ref()
                    .map(|flashblocks_components_ctx| {
                        flashblocks_components_ctx.flashblocks_state.clone()
                    }),
                components_context
                    .as_ref()
                    .map(|flashblocks_components_ctx| {
                        flashblocks_components_ctx
                            .to_jobs_generator
                            .clone()
                            .subscribe()
                    }),
                spoof_authorizer_sk,
                Duration::from_millis(flashblocks_interval),
                Duration::from_millis(flashblocks_recommit_interval),
            ))
            .network(fb_network_builder)
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
        let op_eth_api_builder =
            OpEthApiBuilder::default().with_sequencer(self.config.args.rollup.sequencer.clone());

        let maybe_pending_block =
            self.components_context
                .as_ref()
                .map(|flashblocks_components_ctx| {
                    flashblocks_components_ctx.flashblocks_state.pending_block()
                });
        let flashblocks_eth_api_builder =
            FlashblocksEthApiBuilder::new(op_eth_api_builder, maybe_pending_block);

        let rpc_add_ons = RpcAddOns::new(
            flashblocks_eth_api_builder,
            Default::default(),
            engine_api_builder,
            Default::default(),
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

#[derive(Clone, Debug)]
pub struct FlashblocksComponentsContext {
    pub flashblocks_handle: FlashblocksHandle,
    pub flashblocks_state: FlashblocksExecutionCoordinator,
    pub to_jobs_generator: tokio::sync::watch::Sender<Option<Authorization>>,
    pub authorizer_vk: VerifyingKey,
}

impl From<WorldChainNodeConfig> for FlashblocksContext {
    fn from(value: WorldChainNodeConfig) -> Self {
        let components_context = value
            .args
            .flashblocks
            .as_ref()
            .map(|_flashblocks_args| value.clone().into());
        Self {
            config: value,
            components_context,
        }
    }
}

impl From<WorldChainNodeConfig> for FlashblocksComponentsContext {
    fn from(value: WorldChainNodeConfig) -> Self {
        let flashblocks = value
            .args
            .flashblocks
            .expect("Flashblocks args must be present");

        let authorizer_vk = flashblocks.authorizer_vk.unwrap_or(
            flashblocks
                .builder_sk
                .as_ref()
                .expect("flashblocks builder_sk required")
                .verifying_key(),
        );
        let builder_sk = flashblocks.builder_sk.clone();
        let flashblocks_handle = FlashblocksHandle::new(authorizer_vk, builder_sk.clone());

        let (pending_block, _) = tokio::sync::watch::channel(None);

        let flashblocks_state =
            FlashblocksExecutionCoordinator::new(flashblocks_handle.clone(), pending_block);

        let (to_jobs_generator, _) = tokio::sync::watch::channel(None);

        Self {
            flashblocks_state,
            flashblocks_handle,
            to_jobs_generator,
            authorizer_vk,
        }
    }
}
