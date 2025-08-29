// Module defining World Chain Node Preset contexts for components & add-ons.

use crate::{
    args::WorldChainArgs,
    flashblocks::{
        payload_builder_builder::FlashblocksPayloadBuilderBuilder,
        rpc::FlashblocksEngineApiBuilder, service_builder::FlashblocksPayloadServiceBuilder,
    },
    node::{
        WorldChainNode, WorldChainNodeComponentBuilder, WorldChainNodeConfig,
        WorldChainNodeContext, WorldChainPayloadBuilderBuilder, WorldChainPoolBuilder,
    },
};
use flashblocks_p2p::{net::FlashblocksNetworkBuilder, protocol::handler::FlashblocksHandle};
use reth::rpc::compat::RpcTypes;
use reth_node_api::{FullNodeTypes, NodeTypes};
use reth_node_builder::{
    components::{BasicPayloadServiceBuilder, ComponentsBuilder, PayloadServiceBuilder},
    rpc::BasicEngineValidatorBuilder,
    NodeAdapter, NodeComponentsBuilder,
};
use reth_optimism_evm::OpEvmConfig;
use reth_optimism_node::{
    args::RollupArgs, OpAddOns, OpAddOnsBuilder, OpConsensusBuilder, OpEngineApiBuilder,
    OpEngineValidatorBuilder, OpExecutorBuilder, OpNetworkBuilder,
};
use reth_optimism_rpc::OpEthApiBuilder;
use rollup_boost::{
    ed25519_dalek::{SigningKey, VerifyingKey},
    Authorization,
};

use world_chain_builder_flashblocks::{
    builder::executor::FlashblocksStateExecutor, rpc::eth::FlashblocksEthApiBuilder,
};
use world_chain_builder_pool::BasicWorldChainPool;
use world_chain_provider::InMemoryState;

#[derive(Clone, Debug)]
pub struct BasicContext(WorldChainNodeConfig);

impl From<WorldChainNodeConfig> for BasicContext {
    fn from(value: WorldChainNodeConfig) -> Self {
        Self(value)
    }
}

impl<N: FullNodeTypes<Provider: InMemoryState, Types = WorldChainNode<BasicContext>>>
    WorldChainNodeContext<N> for BasicContext
where
    BasicPayloadServiceBuilder<WorldChainPayloadBuilderBuilder>: PayloadServiceBuilder<
        N,
        BasicWorldChainPool<N>,
        OpEvmConfig<<<N as FullNodeTypes>::Types as NodeTypes>::ChainSpec>,
    >,
{
    type Net = OpNetworkBuilder;
    type Evm = OpEvmConfig;
    type PayloadServiceBuilder = BasicPayloadServiceBuilder<WorldChainPayloadBuilderBuilder>;

    type ComponentsBuilder = WorldChainNodeComponentBuilder<N, Self>;

    type AddOns = OpAddOns<
        NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>,
        OpEthApiBuilder,
        OpEngineValidatorBuilder,
        OpEngineApiBuilder<OpEngineValidatorBuilder>,
    >;

    type ExtContext = ();

    fn components(&self) -> Self::ComponentsBuilder {
        let Self(WorldChainNodeConfig {
            args: WorldChainArgs {
                rollup, builder, ..
            },
            da_config,
        }) = self.clone();

        let RollupArgs {
            disable_txpool_gossip,
            compute_pending_block,
            discovery_v4,
            ..
        } = rollup;

        ComponentsBuilder::default()
            .node_types::<N>()
            .pool(WorldChainPoolBuilder::new(
                builder.pbh_entrypoint,
                builder.signature_aggregator,
                builder.world_id,
            ))
            .executor(OpExecutorBuilder::default())
            .payload(BasicPayloadServiceBuilder::new(
                WorldChainPayloadBuilderBuilder::new(
                    compute_pending_block,
                    builder.verified_blockspace_capacity,
                    builder.pbh_entrypoint,
                    builder.signature_aggregator,
                    builder.private_key,
                )
                .with_da_config(da_config),
            ))
            .network(OpNetworkBuilder {
                disable_txpool_gossip,
                disable_discovery_v4: !discovery_v4,
            })
            .consensus(OpConsensusBuilder::default())
    }

    fn add_ons(&self) -> Self::AddOns {
        Self::AddOns::builder()
            .with_sequencer(self.0.args.rollup.sequencer.clone())
            .with_da_config(self.0.da_config.clone())
            .build()
    }

    fn ext_context(&self) -> Self::ExtContext {}
}

#[derive(Clone, Debug)]
pub struct FlashblocksContext {
    config: WorldChainNodeConfig,
    components_context: FlashblocksComponentsContext,
}

impl<N: FullNodeTypes<Provider: InMemoryState, Types = WorldChainNode<FlashblocksContext>>>
    WorldChainNodeContext<N> for FlashblocksContext
where
    FlashblocksPayloadServiceBuilder<FlashblocksPayloadBuilderBuilder>: PayloadServiceBuilder<
        N,
        BasicWorldChainPool<N>,
        OpEvmConfig<<<N as FullNodeTypes>::Types as NodeTypes>::ChainSpec>,
    >,
{
    type Net = FlashblocksNetworkBuilder<OpNetworkBuilder>;
    type Evm = OpEvmConfig;
    type PayloadServiceBuilder = FlashblocksPayloadServiceBuilder<FlashblocksPayloadBuilderBuilder>;

    type ComponentsBuilder = WorldChainNodeComponentBuilder<N, Self>;

    type AddOns = OpAddOns<
        NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>,
        FlashblocksEthApiBuilder,
        // OpEthApiBuilder,
        OpEngineValidatorBuilder,
        FlashblocksEngineApiBuilder<OpEngineValidatorBuilder>,
        BasicEngineValidatorBuilder<OpEngineValidatorBuilder>,
    >;

    type ExtContext = FlashblocksComponentsContext;

    fn components(&self) -> Self::ComponentsBuilder {
        let Self {
            config:
                WorldChainNodeConfig {
                    args:
                        WorldChainArgs {
                            rollup,
                            builder,
                            flashblocks,
                        },
                    da_config,
                },
            components_context,
        } = self.clone();

        let RollupArgs {
            disable_txpool_gossip,
            compute_pending_block,
            discovery_v4,
            ..
        } = rollup;

        let op_network_builder = OpNetworkBuilder {
            disable_txpool_gossip,
            disable_discovery_v4: !discovery_v4,
        };

        let fb_network_builder = FlashblocksNetworkBuilder::new(
            op_network_builder,
            components_context.flashblocks_handle.clone(),
        );

        let flashblocks = flashblocks.unwrap();

        ComponentsBuilder::default()
            .node_types::<N>()
            .pool(WorldChainPoolBuilder::new(
                builder.pbh_entrypoint,
                builder.signature_aggregator,
                builder.world_id,
            ))
            .executor(OpExecutorBuilder::default())
            .payload(FlashblocksPayloadServiceBuilder::new(
                FlashblocksPayloadBuilderBuilder::new(
                    compute_pending_block,
                    builder.verified_blockspace_capacity,
                    builder.pbh_entrypoint,
                    builder.signature_aggregator,
                    builder.private_key.clone(),
                    components_context.flashblocks_state.clone(),
                )
                .with_da_config(da_config.clone()),
                components_context.flashblocks_handle.clone(),
                components_context.flashblocks_state.clone(),
                components_context.to_jobs_generator.clone().subscribe(),
                flashblocks.builder_sk.clone(),
            ))
            .network(fb_network_builder)
            .executor(OpExecutorBuilder::default())
            .consensus(OpConsensusBuilder::default())
    }

    fn add_ons(&self) -> Self::AddOns {
        todo!()
        // self.add_ons_builder()
        //     .build::<_, _, FlashblocksEngineApiBuilder<OpEngineValidatorBuilder>, _>()
        //     .with_engine_api(self.engine_api_builder())
    }

    fn ext_context(&self) -> Self::ExtContext {
        self.components_context.clone()
    }
}

impl FlashblocksContext {
    fn add_ons_builder<NetworkT: RpcTypes>(&self) -> OpAddOnsBuilder<NetworkT> {
        let Self {
            config:
                WorldChainNodeConfig {
                    args:
                        WorldChainArgs {
                            rollup: rollup_args,
                            ..
                        },
                    da_config,
                },
            components_context: _,
        } = self;

        OpAddOnsBuilder::default()
            .with_sequencer(rollup_args.sequencer.clone())
            .with_sequencer_headers(rollup_args.sequencer_headers.clone())
            .with_da_config(da_config.clone())
            .with_enable_tx_conditional(rollup_args.enable_tx_conditional)
            .with_min_suggested_priority_fee(rollup_args.min_suggested_priority_fee)
            .with_historical_rpc(rollup_args.historical_rpc.clone())
    }

    /// Returns the [`WorldChainEngineApiBuilder`] for the World Chain node.
    fn engine_api_builder(&self) -> FlashblocksEngineApiBuilder<OpEngineValidatorBuilder> {
        let Self {
            components_context, ..
        } = self;

        FlashblocksEngineApiBuilder {
            engine_validator_builder: OpEngineValidatorBuilder::default(),
            flashblocks_handle: Some(components_context.flashblocks_handle.clone()),
            // flashblocks_state: Some(components_context.flashblocks_state.clone()),
            to_jobs_generator: components_context.to_jobs_generator.clone(),
            authorizer_vk: components_context.authorizer_vk,
        }
    }
}
#[derive(Clone, Debug)]
pub struct FlashblocksComponentsContext {
    pub flashblocks_handle: FlashblocksHandle,
    pub flashblocks_state: FlashblocksStateExecutor,
    pub to_jobs_generator: tokio::sync::watch::Sender<Option<Authorization>>,
    pub authorizer_vk: VerifyingKey,
    pub builder_sk: SigningKey,
}

impl From<WorldChainNodeConfig> for FlashblocksContext {
    fn from(value: WorldChainNodeConfig) -> Self {
        Self {
            config: value.clone(),
            components_context: value.into(),
        }
    }
}

impl From<WorldChainNodeConfig> for FlashblocksComponentsContext {
    fn from(value: WorldChainNodeConfig) -> Self {
        let flashblocks = value
            .args
            .flashblocks
            .expect("Flashblocks args must be present");

        let authorizer_vk = flashblocks
            .authorizor_vk
            .unwrap_or(flashblocks.builder_sk.verifying_key());
        let builder_sk = flashblocks.builder_sk.clone();
        let flashblocks_handle = FlashblocksHandle::new(authorizer_vk, builder_sk.clone());

        let flashblocks_state =
            FlashblocksStateExecutor::new(flashblocks_handle.clone(), value.da_config.clone());

        let (to_jobs_generator, _) = tokio::sync::watch::channel(None);
        Self {
            flashblocks_state,
            flashblocks_handle,
            to_jobs_generator,
            authorizer_vk,
            builder_sk,
        }
    }
}
