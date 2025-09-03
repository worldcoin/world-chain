//! OP-Reth `eth_` endpoint implementation.

pub mod receipt;
pub mod transaction;
pub mod core;

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

use crate::builder::executor::FlashblocksStateExecutor;

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
pub struct FlashblocksEthApi<T>
where
    T: Clone,
{
    inner: T,
}

impl<T> FlashblocksEthApi<T>
where
    T: Clone,
{
    pub fn new(inner: T) -> Self {
        Self { inner }
    }
}

impl<T> EthApiTypes for FlashblocksEthApi<T>
where
    T: EthApiTypes,
{
    type Error = T::Error;
    type NetworkTypes = T::NetworkTypes;
    type RpcConvert = T::RpcConvert;

    fn tx_resp_builder(&self) -> &Self::RpcConvert {
        self.inner.tx_resp_builder()
    }
}

impl<T> RpcNodeCore for FlashblocksEthApi<T>
where
    T: RpcNodeCore,
{
    type Primitives = T::Primitives;
    type Provider = T::Provider;
    type Pool = T::Pool;
    type Evm = T::Evm;
    type Network = T::Network;

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

impl<T> RpcNodeCoreExt for FlashblocksEthApi<T>
where
    T: RpcNodeCoreExt,
{
    #[inline]
    fn cache(&self) -> &EthStateCache<T::Primitives> {
        self.inner.cache()
    }
}

impl<T> EthApiSpec for FlashblocksEthApi<T>
where
    T: EthApiSpec,
{
    type Transaction = T::Transaction;
    type Rpc = T::Rpc;

    #[inline]
    fn starting_block(&self) -> U256 {
        self.inner.starting_block()
    }

    #[inline]
    fn signers(&self) -> &SignersForApi<Self> {
        self.inner.signers()
    }
}

impl<T> SpawnBlocking for FlashblocksEthApi<T>
where
    T: SpawnBlocking,
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

impl<T> LoadFee for FlashblocksEthApi<T>
where
    T: LoadFee,
{
    #[inline]
    fn gas_oracle(&self) -> &GasPriceOracle<Self::Provider> {
        self.inner.gas_oracle()
    }

    #[inline]
    fn fee_history_cache(&self) -> &FeeHistoryCache<ProviderHeader<T::Provider>> {
        self.inner.fee_history_cache()
    }

    async fn suggested_priority_fee(&self) -> Result<U256, Self::Error> {
        LoadFee::suggested_priority_fee(&self.inner).await
    }
}

impl<T> LoadState for FlashblocksEthApi<T> where T: LoadState + Clone + SpawnBlocking {}

impl<T> EthState for FlashblocksEthApi<T>
where
    T: EthState + Clone,
{
    #[inline]
    fn max_proof_window(&self) -> u64 {
        self.inner.max_proof_window()
    }
}

impl<T> EthFees for FlashblocksEthApi<T> where T: EthFees + Clone {}

impl<T> Trace for FlashblocksEthApi<T> where T: Trace + Clone + SpawnBlocking {}

impl<T> AddDevSigners for FlashblocksEthApi<T>
where
    T: AddDevSigners + Clone,
{
    fn with_dev_accounts(&self) {
        self.inner.with_dev_accounts()
    }
}

impl<T> std::fmt::Debug for FlashblocksEthApi<T>
where
    T: Clone,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlashblocksEthApi").finish_non_exhaustive()
    }
}

/// Builds [`OpEthApi`] for Optimism.
#[derive(Debug)]
pub struct FlashblocksEthApiBuilder<NetworkT = Optimism> {
    inner: OpEthApiBuilder<NetworkT>,
    state_executor: FlashblocksStateExecutor,
}

impl<NetworkT> Default for FlashblocksEthApiBuilder<NetworkT> {
    fn default() -> Self {
        Self {
            inner: OpEthApiBuilder::default(),
            state_executor: FlashblocksStateExecutor::default(),
        }
    }
}

impl<NetworkT> FlashblocksEthApiBuilder<NetworkT> {
    /// Creates a [`OpEthApiBuilder`] instance from core components.
    pub const fn new(
        inner: OpEthApiBuilder<NetworkT>,
        state_executor: FlashblocksStateExecutor,
    ) -> Self {
        Self {
            inner,
            state_executor,
        }
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
    FlashblocksEthApi<OpEthApi<N, OpRpcConvert<N, NetworkT>>>:
        FullEthApiServer<Provider = N::Provider, Pool = N::Pool> + AddDevSigners,
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
    type EthApi = FlashblocksEthApi<OpEthApi<N, OpRpcConvert<N, NetworkT>>>;

    async fn build_eth_api(self, ctx: EthApiCtx<'_, N>) -> eyre::Result<Self::EthApi> {
        let inner = self.inner.build_eth_api(ctx).await?;
        Ok(FlashblocksEthApi::new(inner))
    }
}
