//! OP-Reth `eth_` endpoint implementation.

pub mod receipt;
pub mod transaction;

mod block;
mod call;
mod pending_block;
use alloy_primitives::U256;
use op_alloy_network::AnyNetwork;
use reth_chainspec::ChainSpec;
use reth_evm::ConfigureEvm;
use reth_network_api::NetworkInfo;
use reth_node_api::{BuilderProvider, FullNodeComponents, NodeTypes};
use reth_node_builder::EthApiBuilderCtx;
use reth_optimism_rpc::{OpEthApi, OpEthApiError};
use reth_primitives::Header;
use reth_provider::{
    BlockIdReader, BlockNumReader, BlockReaderIdExt, CanonStateSubscriptions, ChainSpecProvider,
    HeaderProvider, StageCheckpointReader, StateProviderFactory,
};
use reth_rpc::eth::DevSigner;
use reth_rpc_eth_api::{
    helpers::{
        AddDevSigners, EthApiSpec, EthFees, EthSigner, EthState, LoadBlock, LoadFee, LoadState,
        SpawnBlocking, Trace,
    },
    EthApiTypes,
};
use reth_rpc_eth_types::{EthStateCache, FeeHistoryCache, GasPriceOracle};
use reth_tasks::{
    pool::{BlockingTaskGuard, BlockingTaskPool},
    TaskSpawner,
};
use reth_transaction_pool::TransactionPool;
use std::fmt;

/// OP-Reth `Eth` API implementation.
///
/// This type provides the functionality for handling `eth_` related requests.
///
/// This wraps a default `Eth` implementation, and provides additional functionality where the
/// optimism spec deviates from the default (ethereum) spec, e.g. transaction forwarding to the
/// sequencer, receipts, additional RPC fields for transaction receipts.
///
/// This type implements the [`FullEthApi`](reth_rpc_eth_api::helpers::FullEthApi) by implemented
/// all the `Eth` helper traits and prerequisite traits.
#[derive(Clone)]
pub struct WorldChainEthApi<N: FullNodeComponents> {
    inner: OpEthApi<N>,
}

impl<N> WorldChainEthApi<N>
where
    N: FullNodeComponents<
        Provider: BlockReaderIdExt + ChainSpecProvider + CanonStateSubscriptions + Clone + 'static,
    >,
{
    /// Creates a new instance for given context.
    #[allow(clippy::type_complexity)]
    pub fn with_spawner(ctx: &EthApiBuilderCtx<N>) -> Self {
        let inner = OpEthApi::with_spawner(ctx);
        Self { inner }
    }
}

impl<N> EthApiTypes for WorldChainEthApi<N>
where
    Self: Send + Sync,
    N: FullNodeComponents,
{
    type Error = OpEthApiError;
    type NetworkTypes = AnyNetwork;
}

impl<N> EthApiSpec for WorldChainEthApi<N>
where
    Self: Send + Sync,
    N: FullNodeComponents<Types: NodeTypes<ChainSpec = ChainSpec>>,
{
    #[inline]
    fn provider(
        &self,
    ) -> impl ChainSpecProvider<ChainSpec = ChainSpec> + BlockNumReader + StageCheckpointReader
    {
        EthApiSpec::provider(&self.inner)
    }

    #[inline]
    fn network(&self) -> impl NetworkInfo {
        self.inner.network()
    }

    #[inline]
    fn starting_block(&self) -> U256 {
        self.inner.starting_block()
    }

    #[inline]
    fn signers(&self) -> &parking_lot::RwLock<Vec<Box<dyn EthSigner>>> {
        self.inner.signers()
    }
}

impl<N> SpawnBlocking for WorldChainEthApi<N>
where
    Self: Send + Sync + Clone + 'static,
    N: FullNodeComponents,
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

impl<N> LoadFee for WorldChainEthApi<N>
where
    Self: LoadBlock,
    N: FullNodeComponents<Types: NodeTypes<ChainSpec = ChainSpec>>,
{
    #[inline]
    fn provider(
        &self,
    ) -> impl BlockIdReader + HeaderProvider + ChainSpecProvider<ChainSpec = ChainSpec> {
        LoadFee::provider(&self.inner)
    }

    #[inline]
    fn cache(&self) -> &EthStateCache {
        LoadFee::cache(&self.inner)
    }

    #[inline]
    fn gas_oracle(&self) -> &GasPriceOracle<impl BlockReaderIdExt> {
        self.inner.gas_oracle()
    }

    #[inline]
    fn fee_history_cache(&self) -> &FeeHistoryCache {
        self.inner.fee_history_cache()
    }
}

impl<N> LoadState for WorldChainEthApi<N>
where
    Self: Send + Sync,
    N: FullNodeComponents<Types: NodeTypes<ChainSpec = ChainSpec>>,
{
    #[inline]
    fn provider(&self) -> impl StateProviderFactory + ChainSpecProvider<ChainSpec = ChainSpec> {
        LoadState::provider(&self.inner)
    }

    #[inline]
    fn cache(&self) -> &EthStateCache {
        LoadState::cache(&self.inner)
    }

    #[inline]
    fn pool(&self) -> impl TransactionPool {
        self.inner.pool()
    }
}

impl<N> EthState for WorldChainEthApi<N>
where
    Self: LoadState,
    OpEthApi<N>: LoadState,
    N: FullNodeComponents,
{
    #[inline]
    fn max_proof_window(&self) -> u64 {
        self.inner.max_proof_window()
    }
}

impl<N> EthFees for WorldChainEthApi<N>
where
    Self: LoadFee,
    N: FullNodeComponents,
{
}

impl<N> Trace for WorldChainEthApi<N>
where
    Self: LoadState,
    OpEthApi<N>: LoadState,
    N: FullNodeComponents,
{
    #[inline]
    fn evm_config(&self) -> &impl ConfigureEvm<Header = Header> {
        self.inner.evm_config()
    }
}

impl<N: FullNodeComponents<Types: NodeTypes<ChainSpec = ChainSpec>>> AddDevSigners
    for WorldChainEthApi<N>
{
    fn with_dev_accounts(&self) {
        *self.signers().write() = DevSigner::random_signers(20)
    }
}

impl<N> BuilderProvider<N> for WorldChainEthApi<N>
where
    Self: Send,
    N: FullNodeComponents,
{
    type Ctx<'a> = &'a EthApiBuilderCtx<N>;

    fn builder() -> Box<dyn for<'a> Fn(Self::Ctx<'a>) -> Self + Send> {
        Box::new(|ctx| Self::with_spawner(ctx))
    }
}

impl<N: FullNodeComponents> fmt::Debug for WorldChainEthApi<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpEthApi").finish_non_exhaustive()
    }
}
