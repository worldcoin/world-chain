//! OP-Reth `eth_` endpoint implementation.

pub mod receipt;
pub mod transaction;

mod block;
mod call;
mod pending_block;

use alloy_primitives::U256;
use op_alloy_network::Optimism;
use op_alloy_rpc_types_engine::OpFlashblockPayloadBase;
use reth_chain_state::ExecutedBlock;
use reth_chainspec::{EthereumHardforks, Hardforks};
use reth_evm::ConfigureEvm;
use reth_node_api::{FullNodeComponents, HeaderTy, NodeTypes};
use reth_node_builder::rpc::{EthApiBuilder, EthApiCtx};
use reth_optimism_flashblocks::FlashBlockCompleteSequence;
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::{OpEthApi, OpEthApiBuilder, OpEthApiError, eth::OpRpcConvert};
use reth_rpc_eth_api::{
    EthApiTypes, FromEvmError, FullEthApiServer, RpcConvert, RpcNodeCore, RpcNodeCoreExt, RpcTypes,
    helpers::{
        EthApiSpec, EthFees, EthState, LoadFee, LoadPendingBlock, LoadState, SpawnBlocking, Trace,
        pending_block::BuildPendingEnv,
    },
};
use reth_rpc_eth_types::{EthStateCache, FeeHistoryCache, GasPriceOracle};
use reth_storage_api::ProviderHeader;
use reth_tasks::{
    TaskSpawner,
    pool::{BlockingTaskGuard, BlockingTaskPool},
};

/// Flashblocks `Eth` API implementation.
///
/// This type provides the functionality for handling `eth_` related requests.
///
/// This wraps a default `Op-Eth` implementation, and provides additional functionality where the
/// flashblocks spec deviates from the default (optimism) spec
///
/// This type implements the [`FullEthApi`](reth_rpc_eth_api::helpers::FullEthApi) by implemented
/// all the `Eth` helper traits and prerequisite traits.
#[derive(Clone)]
pub struct FlashblocksEthApi<N: RpcNodeCore, Rpc: RpcConvert> {
    inner: OpEthApi<N, Rpc>,
    pending_block: Option<tokio::sync::watch::Receiver<Option<ExecutedBlock<OpPrimitives>>>>,
}

impl<N, Rpc> FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert + Clone,
{
    pub fn new(
        inner: OpEthApi<N, Rpc>,
        pending_block: Option<tokio::sync::watch::Receiver<Option<ExecutedBlock<OpPrimitives>>>>,
    ) -> Self {
        Self {
            inner,
            pending_block,
        }
    }
}

impl<N, Rpc> EthApiTypes for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert + Clone,
    OpEthApi<N, Rpc>: EthApiTypes,
{
    type Error = <OpEthApi<N, Rpc> as EthApiTypes>::Error;
    type NetworkTypes = <OpEthApi<N, Rpc> as EthApiTypes>::NetworkTypes;
    type RpcConvert = <OpEthApi<N, Rpc> as EthApiTypes>::RpcConvert;

    fn converter(&self) -> &Self::RpcConvert {
        self.inner.converter()
    }
}

impl<N, Rpc> RpcNodeCore for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert + Clone,
    OpEthApi<N, Rpc>: RpcNodeCore,
{
    type Primitives = <OpEthApi<N, Rpc> as RpcNodeCore>::Primitives;
    type Provider = <OpEthApi<N, Rpc> as RpcNodeCore>::Provider;
    type Pool = <OpEthApi<N, Rpc> as RpcNodeCore>::Pool;
    type Evm = <OpEthApi<N, Rpc> as RpcNodeCore>::Evm;
    type Network = <OpEthApi<N, Rpc> as RpcNodeCore>::Network;

    #[inline]
    fn pool(&self) -> &Self::Pool {
        self.inner.pool()
    }

    #[inline]
    fn evm_config(&self) -> &Self::Evm {
        self.inner.evm_config()
    }

    #[inline]
    fn network(&self) -> &Self::Network {
        self.inner.network()
    }

    #[inline]
    fn provider(&self) -> &Self::Provider {
        self.inner.provider()
    }
}

impl<N, Rpc> RpcNodeCoreExt for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert + Clone,
    OpEthApi<N, Rpc>: RpcNodeCoreExt,
{
    #[inline]
    fn cache(&self) -> &EthStateCache<<OpEthApi<N, Rpc> as RpcNodeCore>::Primitives> {
        self.inner.cache()
    }
}

impl<N, Rpc> EthApiSpec for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert + Clone,
    OpEthApi<N, Rpc>: EthApiSpec,
{
    #[inline]
    fn starting_block(&self) -> U256 {
        self.inner.starting_block()
    }
}

impl<N, Rpc> SpawnBlocking for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert + Clone,
    OpEthApi<N, Rpc>: SpawnBlocking,
{
    #[inline]
    fn io_task_spawner(&self) -> impl TaskSpawner {
        self.inner.io_task_spawner()
    }

    #[inline]
    fn tracing_task_pool(&self) -> &BlockingTaskPool {
        self.inner.tracing_task_pool()
    }

    #[inline]
    fn tracing_task_guard(&self) -> &BlockingTaskGuard {
        self.inner.tracing_task_guard()
    }
}

impl<N, Rpc> LoadFee for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Primitives = OpPrimitives>,
    Rpc: RpcConvert + Clone,
    OpEthApi<N, Rpc>:
        RpcNodeCore<Primitives = OpPrimitives> + EthApiTypes<Error = OpEthApiError> + LoadFee,
    OpEthApiError: FromEvmError<N::Evm>,
{
    #[inline]
    fn gas_oracle(&self) -> &GasPriceOracle<Self::Provider> {
        self.inner.gas_oracle()
    }

    #[inline]
    fn fee_history_cache(
        &self,
    ) -> &FeeHistoryCache<ProviderHeader<<OpEthApi<N, Rpc> as RpcNodeCore>::Provider>> {
        self.inner.fee_history_cache()
    }

    async fn suggested_priority_fee(&self) -> Result<U256, Self::Error> {
        LoadFee::suggested_priority_fee(&self.inner).await
    }
}

impl<N: RpcNodeCore<Primitives = OpPrimitives>, Rpc: RpcConvert + Clone> LoadState
    for FlashblocksEthApi<N, Rpc>
where
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: RpcNodeCore<Primitives = OpPrimitives>
        + LoadPendingBlock
        + EthApiTypes<Error = OpEthApiError>
        + LoadState
        + Clone
        + SpawnBlocking,
{
}

impl<N, Rpc> EthState for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Primitives = OpPrimitives>,
    Rpc: RpcConvert + Clone,
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: RpcNodeCore<Primitives = OpPrimitives>
        + EthApiTypes<Error = OpEthApiError>
        + EthState
        + Clone,
{
    #[inline]
    fn max_proof_window(&self) -> u64 {
        self.inner.max_proof_window()
    }
}

impl<N, Rpc> EthFees for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Primitives = OpPrimitives>,
    Rpc: RpcConvert + Clone,
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: RpcNodeCore<Primitives = OpPrimitives>
        + EthApiTypes<Error = OpEthApiError>
        + EthFees
        + Clone,
{
}

impl<N, Rpc> Trace for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Primitives = OpPrimitives>,
    Rpc: RpcConvert + Clone,
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: Trace
        + SpawnBlocking
        + RpcNodeCore<Primitives = OpPrimitives>
        + EthApiTypes<Error = OpEthApiError>
        + Clone,
{
}

impl<N, Rpc> std::fmt::Debug for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert,
    OpEthApi<N, Rpc>: Clone,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlashblocksEthApi").finish_non_exhaustive()
    }
}

/// Builds [`OpEthApi`] for Optimism.
#[derive(Debug)]
pub struct FlashblocksEthApiBuilder<NetworkT = Optimism> {
    inner: OpEthApiBuilder<NetworkT>,
    pending_block: Option<tokio::sync::watch::Receiver<Option<ExecutedBlock<OpPrimitives>>>>,
}

impl<NetworkT> Default for FlashblocksEthApiBuilder<NetworkT> {
    fn default() -> Self {
        Self {
            inner: OpEthApiBuilder::default(),
            pending_block: None,
        }
    }
}

impl<NetworkT> FlashblocksEthApiBuilder<NetworkT> {
    /// Creates a [`OpEthApiBuilder`] instance from core components.
    pub const fn new(
        inner: OpEthApiBuilder<NetworkT>,
        pending_block: Option<tokio::sync::watch::Receiver<Option<ExecutedBlock<OpPrimitives>>>>,
    ) -> Self {
        Self {
            inner,
            pending_block,
        }
    }
}

impl<N, NetworkT> EthApiBuilder<N> for FlashblocksEthApiBuilder<NetworkT>
where
    N: FullNodeComponents<
            Evm: ConfigureEvm<
                NextBlockEnvCtx: BuildPendingEnv<HeaderTy<N::Types>>
                                     + From<OpFlashblockPayloadBase>
                                     + Unpin,
            >,
            Types: NodeTypes<
                ChainSpec: Hardforks + EthereumHardforks,
                Payload: reth_node_api::PayloadTypes<
                    ExecutionData: for<'a> TryFrom<
                        &'a FlashBlockCompleteSequence,
                        Error: std::fmt::Display,
                    >,
                >,
            >,
        >,
    NetworkT: RpcTypes,
    OpRpcConvert<N, NetworkT>: RpcConvert<Network = NetworkT> + Clone,
    OpEthApi<N, OpRpcConvert<N, NetworkT>>:
        FullEthApiServer<Provider = N::Provider, Pool = N::Pool>,
    FlashblocksEthApi<N, OpRpcConvert<N, NetworkT>>:
        FullEthApiServer<Provider = N::Provider, Pool = N::Pool>,
    OpEthApiBuilder<NetworkT>: EthApiBuilder<N, EthApi = OpEthApi<N, OpRpcConvert<N, NetworkT>>>,
{
    type EthApi = FlashblocksEthApi<N, OpRpcConvert<N, NetworkT>>;

    async fn build_eth_api(self, ctx: EthApiCtx<'_, N>) -> eyre::Result<Self::EthApi> {
        let inner = self.inner.build_eth_api(ctx).await?;
        let pending_block = self.pending_block;
        Ok(FlashblocksEthApi::new(inner, pending_block))
    }
}
