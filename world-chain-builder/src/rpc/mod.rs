//! OP-Reth `eth_` endpoint implementation.

mod block;
pub mod bundle;
mod call;
mod pending_block;
pub mod receipt;
pub mod transaction;
use alloy_primitives::U256;
use alloy_rpc_types::TransactionInfo;
use derive_more::derive::Deref;
use op_alloy_network::Optimism;
use op_alloy_rpc_types::Transaction;
use reth::api::{ConfigureEvm, FullNodeComponents};
use reth::builder::EthApiBuilderCtx;
use reth::chainspec::{EthChainSpec, EthereumHardforks};

use reth::network::NetworkInfo;
use reth::rpc::api::eth::helpers::{
    AddDevSigners, EthApiSpec, EthFees, EthState, LoadBlock, LoadFee, LoadState, SpawnBlocking,
    Trace,
};

use reth::rpc::api::eth::RpcNodeCoreExt;
use reth::rpc::compat::TransactionCompat;
use reth::rpc::eth::{EthApiTypes, RpcNodeCore};
use reth::rpc::server_types::eth::{EthStateCache, FeeHistoryCache, GasPriceOracle};
use reth::tasks::{
    pool::{BlockingTaskGuard, BlockingTaskPool},
    TaskSpawner,
};
use reth::transaction_pool::TransactionPool;
use reth_optimism_rpc::{OpEthApi, OpEthApiError};
use reth_primitives::{Header, TransactionSignedEcRecovered};
use reth_provider::{
    BlockNumReader, BlockReaderIdExt, CanonStateSubscriptions, ChainSpecProvider, EvmEnvProvider,
    StageCheckpointReader, StateProviderFactory,
};
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
#[derive(Clone, Deref)]
pub struct WorldChainEthApi<N: RpcNodeCore> {
    #[deref]
    inner: OpEthApi<N>,
}

impl<N> WorldChainEthApi<N>
where
    N: RpcNodeCore<
        Provider: BlockReaderIdExt + ChainSpecProvider + CanonStateSubscriptions + Clone + 'static,
    >,
{
    /// Creates a new instance for given context.
    #[allow(clippy::type_complexity)]
    pub fn new(ctx: &EthApiBuilderCtx<N>, sequencer_http: Option<String>) -> Self {
        let op_builder_ctx = EthApiBuilderCtx::<N> {
            provider: ctx.provider.clone(),
            pool: ctx.pool.clone(),
            network: ctx.network.clone(),
            evm_config: ctx.evm_config.clone(),
            config: ctx.config.clone(),
            executor: ctx.executor.clone(),
            events: ctx.events.clone(),
            cache: ctx.cache.clone(),
        };

        let inner = OpEthApi::new(&op_builder_ctx, sequencer_http);
        Self { inner }
    }
}

impl<N> EthApiTypes for WorldChainEthApi<N>
where
    Self: Send + Sync,
    N: RpcNodeCore,
{
    type Error = OpEthApiError;
    type NetworkTypes = Optimism;
    type TransactionCompat = Self;

    fn tx_resp_builder(&self) -> &Self::TransactionCompat {
        self
    }
}

impl<N> EthApiSpec for WorldChainEthApi<N>
where
    N: RpcNodeCore<
        Provider: ChainSpecProvider<ChainSpec: EthereumHardforks>
                      + BlockNumReader
                      + StageCheckpointReader,
        Network: NetworkInfo,
    >,
{
    #[inline]
    fn starting_block(&self) -> U256 {
        self.inner.starting_block()
    }

    #[inline]
    fn signers(
        &self,
    ) -> &parking_lot::RwLock<Vec<Box<dyn reth::rpc::api::eth::helpers::EthSigner>>> {
        self.inner.signers()
    }
}

impl<N> RpcNodeCore for WorldChainEthApi<N>
where
    N: RpcNodeCore,
{
    type Provider = N::Provider;
    type Pool = N::Pool;
    type Network = <N as RpcNodeCore>::Network;
    type Evm = <N as RpcNodeCore>::Evm;

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

impl<N> SpawnBlocking for WorldChainEthApi<N>
where
    Self: Send + Sync + Clone + 'static,
    N: RpcNodeCore,
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
    Self: LoadBlock<Provider = N::Provider>,
    N: RpcNodeCore<
        Provider: BlockReaderIdExt
                      + EvmEnvProvider
                      + ChainSpecProvider<ChainSpec: EthChainSpec + EthereumHardforks>
                      + StateProviderFactory,
    >,
{
    #[inline]
    fn gas_oracle(&self) -> &GasPriceOracle<Self::Provider> {
        self.inner.gas_oracle()
    }

    #[inline]
    fn fee_history_cache(&self) -> &FeeHistoryCache {
        self.inner.fee_history_cache()
    }
}

impl<N> LoadState for WorldChainEthApi<N> where
    N: RpcNodeCore<
        Provider: StateProviderFactory + ChainSpecProvider<ChainSpec: EthereumHardforks>,
        Pool: TransactionPool,
    >
{
}

impl<N> EthState for WorldChainEthApi<N>
where
    Self: LoadState + SpawnBlocking,
    OpEthApi<N>: LoadState + SpawnBlocking,
    N: RpcNodeCore,
{
    #[inline]
    fn max_proof_window(&self) -> u64 {
        self.inner.max_proof_window()
    }
}

impl<N> EthFees for WorldChainEthApi<N>
where
    Self: LoadFee,
    N: RpcNodeCore,
{
}

impl<N> Trace for WorldChainEthApi<N>
where
    Self: LoadState<Evm: ConfigureEvm<Header = Header>>,
    N: RpcNodeCore,
{
}

impl<N> TransactionCompat for WorldChainEthApi<N>
where
    N: FullNodeComponents,
{
    type Transaction = Transaction;

    fn fill(
        &self,
        tx: TransactionSignedEcRecovered,
        tx_info: TransactionInfo,
    ) -> Self::Transaction {
        self.inner.fill(tx, tx_info)
    }

    fn otterscan_api_truncate_input(tx: &mut Self::Transaction) {
        OpEthApi::<N>::otterscan_api_truncate_input(tx)
    }

    fn tx_type(tx: &Self::Transaction) -> u8 {
        OpEthApi::<N>::tx_type(tx)
    }
}

impl<N> AddDevSigners for WorldChainEthApi<N>
where
    N: RpcNodeCore,
{
    fn with_dev_accounts(&self) {
        self.inner.with_dev_accounts()
    }
}

impl<N> RpcNodeCoreExt for WorldChainEthApi<N>
where
    N: RpcNodeCore,
{
    #[inline]
    fn cache(&self) -> &EthStateCache {
        self.inner.cache()
    }
}

impl<N: RpcNodeCore> fmt::Debug for WorldChainEthApi<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorldChainEthApi").finish_non_exhaustive()
    }
}
