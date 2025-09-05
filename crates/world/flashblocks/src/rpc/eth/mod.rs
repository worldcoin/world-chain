//! OP-Reth `eth_` endpoint implementation.

pub mod core;
pub mod receipt;
pub mod transaction;

mod block;
mod call;
mod pending_block;

use alloy_primitives::U256;
use op_alloy_network::Optimism;
use reth_evm::ConfigureEvm;
use reth_node_api::{FullNodeComponents, FullNodeTypes, HeaderTy};
use reth_node_builder::rpc::{EthApiBuilder, EthApiCtx};
use reth_optimism_rpc::{
    eth::{receipt::OpReceiptConverter, transaction::OpTxInfoMapper, OpRpcConvert},
    OpEthApi, OpEthApiBuilder,
};
use reth_rpc_eth_api::{
    helpers::{
        pending_block::BuildPendingEnv, spec::SignersForApi, AddDevSigners, EthApiSpec, EthFees,
        EthState, LoadFee, LoadState, SpawnBlocking, Trace,
    },
    EthApiTypes, FullEthApiServer, RpcConvert, RpcConverter, RpcNodeCore, RpcNodeCoreExt, RpcTypes,
};
use reth_rpc_eth_types::{EthStateCache, FeeHistoryCache, GasPriceOracle};
use reth_storage_api::ProviderHeader;
use reth_tasks::{
    pool::{BlockingTaskGuard, BlockingTaskPool},
    TaskSpawner,
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
}

impl<N, Rpc> FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert,
{
    pub fn new(inner: OpEthApi<N, Rpc>) -> Self {
        Self { inner }
    }
}

impl<N, Rpc> EthApiTypes for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert,
    OpEthApi<N, Rpc>: EthApiTypes,
{
    type Error = <OpEthApi<N, Rpc> as EthApiTypes>::Error;
    type NetworkTypes = <OpEthApi<N, Rpc> as EthApiTypes>::NetworkTypes;
    type RpcConvert = <OpEthApi<N, Rpc> as EthApiTypes>::RpcConvert;

    fn tx_resp_builder(&self) -> &Self::RpcConvert {
        self.inner.tx_resp_builder()
    }
}

impl<N, Rpc> RpcNodeCore for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert,
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
    Rpc: RpcConvert,
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
    Rpc: RpcConvert,
    OpEthApi<N, Rpc>: EthApiSpec,
{
    type Transaction = <OpEthApi<N, Rpc> as EthApiSpec>::Transaction;
    type Rpc = <OpEthApi<N, Rpc> as EthApiSpec>::Rpc;

    #[inline]
    fn starting_block(&self) -> U256 {
        self.inner.starting_block()
    }

    #[inline]
    fn signers(&self) -> &SignersForApi<Self> {
        self.inner.signers()
    }
}

impl<N, Rpc> SpawnBlocking for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert,
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
    OpEthApi<N, Rpc>: LoadFee,
    N: RpcNodeCore,
    Rpc: RpcConvert,
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

impl<N: RpcNodeCore, Rpc: RpcConvert> LoadState for FlashblocksEthApi<N, Rpc> where
    OpEthApi<N, Rpc>: LoadState + Clone + SpawnBlocking
{
}

impl<N, Rpc> EthState for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert,
    OpEthApi<N, Rpc>: EthState + Clone,
{
    #[inline]
    fn max_proof_window(&self) -> u64 {
        self.inner.max_proof_window()
    }
}

impl<N, Rpc> EthFees for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert,
    OpEthApi<N, Rpc>: EthFees + Clone,
{
}

impl<N, Rpc> Trace for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert,
    OpEthApi<N, Rpc>: Trace + Clone + SpawnBlocking,
{
}

impl<N, Rpc> AddDevSigners for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert,
    OpEthApi<N, Rpc>: AddDevSigners + Clone,
{
    fn with_dev_accounts(&self) {
        self.inner.with_dev_accounts()
    }
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
}

impl<NetworkT> Default for FlashblocksEthApiBuilder<NetworkT> {
    fn default() -> Self {
        Self {
            inner: OpEthApiBuilder::default(),
        }
    }
}

impl<NetworkT> FlashblocksEthApiBuilder<NetworkT> {
    /// Creates a [`OpEthApiBuilder`] instance from core components.
    pub const fn new(inner: OpEthApiBuilder<NetworkT>) -> Self {
        Self { inner }
    }
}

impl<N, NetworkT> EthApiBuilder<N> for FlashblocksEthApiBuilder<NetworkT>
where
    N: FullNodeComponents<
        Evm: ConfigureEvm<NextBlockEnvCtx: BuildPendingEnv<HeaderTy<N::Types>> + Unpin>,
    >,
    NetworkT: RpcTypes,
    OpRpcConvert<N, NetworkT>: RpcConvert<Network = NetworkT>,
    OpEthApi<N, OpRpcConvert<N, NetworkT>>:
        FullEthApiServer<Provider = N::Provider, Pool = N::Pool> + AddDevSigners,
    FlashblocksEthApi<
        N,
        RpcConverter<
            NetworkT,
            <N as FullNodeComponents>::Evm,
            OpReceiptConverter<<N as FullNodeTypes>::Provider>,
            (),
            OpTxInfoMapper<<N as FullNodeTypes>::Provider>,
        >,
    >: FullEthApiServer<Provider = N::Provider, Pool = N::Pool> + AddDevSigners,
    OpEthApiBuilder<NetworkT>: EthApiBuilder<
        N,
        EthApi = OpEthApi<
            N,
            RpcConverter<
                NetworkT,
                <N as FullNodeComponents>::Evm,
                OpReceiptConverter<<N as FullNodeTypes>::Provider>,
                (),
                OpTxInfoMapper<<N as FullNodeTypes>::Provider>,
            >,
        >,
    >,
{
    type EthApi = FlashblocksEthApi<
        N,
        RpcConverter<
            NetworkT,
            <N as FullNodeComponents>::Evm,
            OpReceiptConverter<<N as FullNodeTypes>::Provider>,
            (),
            OpTxInfoMapper<<N as FullNodeTypes>::Provider>,
        >,
    >;

    async fn build_eth_api(self, ctx: EthApiCtx<'_, N>) -> eyre::Result<Self::EthApi> {
        let inner = self.inner.build_eth_api(ctx).await?;
        Ok(FlashblocksEthApi::new(inner))
    }
}
