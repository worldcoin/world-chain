//! TODO: Delete this module when we update reth.
//! https://github.com/paradigmxyz/reth/pull/18141

use crate::flashblocks::rpc::FlashblocksEngineApiBuilder;
use reth_evm::ConfigureEvm;
use reth_node_api::{
    BuildNextEnv, FullNodeComponents, HeaderTy, NodeAddOns, NodeTypes, PayloadTypes, TxTy,
};
use reth_node_builder::rpc::{
    BasicEngineValidatorBuilder, EngineApiBuilder, EngineValidatorBuilder, EthApiBuilder,
    RethRpcMiddleware, RpcHandle,
};
use reth_node_builder::rpc::{EngineValidatorAddOn, PayloadValidatorBuilder, RethRpcAddOns};

use reth_optimism_forks::OpHardforks;
use reth_optimism_node::txpool::OpPooledTx;
use reth_optimism_node::OpAddOns;
use reth_optimism_node::OpPayloadPrimitives;
use reth_optimism_payload_builder::OpAttributes;
use reth_transaction_pool::TransactionPool;
use serde::de::DeserializeOwned;
use tower::layer::util::Identity;
use world_chain_builder_flashblocks::rpc::eth::FlashblocksEthApiBuilder;

/// Add-ons w.r.t. world chain.
///
/// This type provides optimism-specific addons to the node and exposes the RPC server and engine
/// API.
#[derive(Debug)]
pub struct FlashblocksAddOns<
    N: FullNodeComponents,
    EthB: EthApiBuilder<N>,
    PVB,
    EB = FlashblocksEngineApiBuilder<PVB>,
    EVB = BasicEngineValidatorBuilder<PVB>,
    RpcMiddleware = Identity,
> {
    inner: OpAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>,
}

impl<N, EthB, PVB, EB, EVB, RpcMiddleware> FlashblocksAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>
where
    N: FullNodeComponents,
    EthB: EthApiBuilder<N>,
{
    /// Creates a new instance from components.
    pub const fn new(inner: OpAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>) -> Self {
        Self { inner }
    }
}

impl<N, EthB, PVB, EB, EVB, Attrs, RpcMiddleware> NodeAddOns<N>
    for FlashblocksAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>
where
    N: FullNodeComponents<
        Types: NodeTypes<
            ChainSpec: OpHardforks,
            Primitives: OpPayloadPrimitives,
            Payload: PayloadTypes<PayloadBuilderAttributes = Attrs>,
        >,
        Evm: ConfigureEvm<
            NextBlockEnvCtx: BuildNextEnv<
                Attrs,
                HeaderTy<N::Types>,
                <N::Types as NodeTypes>::ChainSpec,
            >,
        >,
        Pool: TransactionPool<Transaction: OpPooledTx>,
    >,
    EthB: EthApiBuilder<N>,
    PVB: Send,
    EB: EngineApiBuilder<N>,
    EVB: EngineValidatorBuilder<N>,
    RpcMiddleware: RethRpcMiddleware,
    Attrs: OpAttributes<Transaction = TxTy<N::Types>, RpcPayloadAttributes: DeserializeOwned>,
{
    type Handle = RpcHandle<N, EthB::EthApi>;

    async fn launch_add_ons(
        self,
        ctx: reth_node_api::AddOnsContext<'_, N>,
    ) -> eyre::Result<Self::Handle> {
        self.inner.launch_add_ons(ctx).await
    }
}

impl<N, EthB, PVB, EB, EVB, Attrs, RpcMiddleware> RethRpcAddOns<N>
    for FlashblocksAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>
where
    N: FullNodeComponents<
        Types: NodeTypes<
            ChainSpec: OpHardforks,
            Primitives: OpPayloadPrimitives,
            Payload: PayloadTypes<PayloadBuilderAttributes = Attrs>,
        >,
        Evm: ConfigureEvm<
            NextBlockEnvCtx: BuildNextEnv<
                Attrs,
                HeaderTy<N::Types>,
                <N::Types as NodeTypes>::ChainSpec,
            >,
        >,
    >,
    <<N as FullNodeComponents>::Pool as TransactionPool>::Transaction: OpPooledTx,
    EthB: EthApiBuilder<N>,
    PVB: PayloadValidatorBuilder<N>,
    EB: EngineApiBuilder<N>,
    EVB: EngineValidatorBuilder<N>,
    RpcMiddleware: RethRpcMiddleware,
    Attrs: OpAttributes<Transaction = TxTy<N::Types>, RpcPayloadAttributes: DeserializeOwned>,
{
    type EthApi = EthB::EthApi;

    fn hooks_mut(&mut self) -> &mut reth_node_builder::rpc::RpcHooks<N, Self::EthApi> {
        self.inner.hooks_mut()
    }
}

impl<N, NetworkT, PVB, EB, EVB> EngineValidatorAddOn<N>
    for FlashblocksAddOns<N, FlashblocksEthApiBuilder<NetworkT>, PVB, EB, EVB>
where
    N: FullNodeComponents,
    FlashblocksEthApiBuilder<NetworkT>: EthApiBuilder<N>,
    PVB: Send,
    EB: EngineApiBuilder<N>,
    EVB: EngineValidatorBuilder<N>,
{
    type ValidatorBuilder = EVB;

    fn engine_validator_builder(&self) -> Self::ValidatorBuilder {
        EngineValidatorAddOn::engine_validator_builder(&self.inner.rpc_add_ons)
    }
}
