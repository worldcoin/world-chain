//! OP-Reth `eth_` endpoint implementation.

pub mod receipt;
pub mod transaction;

mod block;
mod call;
mod pending_block;

use alloy_primitives::U256;
use op_alloy_network::Optimism;
use reth_evm::ConfigureEvm;
use reth_node_api::{FullNodeComponents, HeaderTy};
use reth_node_builder::rpc::{EthApiBuilder, EthApiCtx};
use reth_optimism_rpc::{
    eth::OpRpcConvert, OpEthApi, OpEthApiBuilder, OpEthApiError, SequencerClient,
};
use reth_rpc::eth::core::EthApiInner;
use reth_rpc_eth_api::{
    helpers::{
        pending_block::BuildPendingEnv, spec::SignersForApi, AddDevSigners, EthApiSpec, EthFees,
        EthState, LoadFee, LoadState, SpawnBlocking, Trace,
    },
    EthApiTypes, FromEvmError, FullEthApiServer, RpcConvert, RpcNodeCore, RpcNodeCoreExt, RpcTypes,
    SignableTxRequest,
};
use reth_rpc_eth_types::{EthStateCache, FeeHistoryCache, GasPriceOracle};
use reth_storage_api::{ProviderHeader, ProviderTx};
use reth_tasks::{
    pool::{BlockingTaskGuard, BlockingTaskPool},
    TaskSpawner,
};

/// Adapter for [`EthApiInner`], which holds all the data required to serve core `eth_` API.
pub type EthApiNodeBackend<N, Rpc> = EthApiInner<N, Rpc>;

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
pub struct FlashblocksEthApi<N: RpcNodeCore, Rpc: RpcConvert> {
    /// Gateway to node's core components.
    inner: OpEthApi<N, Rpc>,
}

impl<N: RpcNodeCore, Rpc: RpcConvert> Clone for FlashblocksEthApi<N, Rpc> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<N: RpcNodeCore, Rpc: RpcConvert> FlashblocksEthApi<N, Rpc> {
    /// Creates a new `OpEthApi`.
    pub fn new(inner: OpEthApi<N, Rpc>) -> Self {
        Self { inner }
    }

    /// Returns a reference to the [`EthApiNodeBackend`].
    pub fn eth_api(&self) -> &EthApiNodeBackend<N, Rpc> {
        self.inner.eth_api()
    }
    /// Returns the configured sequencer client, if any.
    pub fn sequencer_client(&self) -> Option<&SequencerClient> {
        self.inner.sequencer_client()
    }

    // /// Build a [`OpEthApi`] using [`OpEthApiBuilder`].
    // pub const fn builder() -> FlashblocksEthApiBuilder<Rpc> {
    //     FlashblocksEthApiBuilder::new()
    // }
}

impl<N, Rpc> EthApiTypes for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert<Primitives = N::Primitives>,
{
    type Error = OpEthApiError;
    type NetworkTypes = Rpc::Network;
    type RpcConvert = Rpc;

    fn tx_resp_builder(&self) -> &Self::RpcConvert {
        self.inner.tx_resp_builder()
    }
}

impl<N, Rpc> RpcNodeCore for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert<Primitives = N::Primitives>,
{
    type Primitives = N::Primitives;
    type Provider = N::Provider;
    type Pool = N::Pool;
    type Evm = N::Evm;
    type Network = N::Network;

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
    Rpc: RpcConvert<Primitives = N::Primitives>,
{
    #[inline]
    fn cache(&self) -> &EthStateCache<N::Primitives> {
        self.inner.cache()
    }
}

impl<N, Rpc> EthApiSpec for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert<Primitives = N::Primitives>,
{
    type Transaction = ProviderTx<Self::Provider>;
    type Rpc = Rpc::Network;

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
    Rpc: RpcConvert<Primitives = N::Primitives>,
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
    N: RpcNodeCore,
    OpEthApiError: FromEvmError<N::Evm>,
    Rpc: RpcConvert<Primitives = N::Primitives, Error = OpEthApiError>,
{
    #[inline]
    fn gas_oracle(&self) -> &GasPriceOracle<Self::Provider> {
        self.inner.gas_oracle()
    }

    #[inline]
    fn fee_history_cache(&self) -> &FeeHistoryCache<ProviderHeader<N::Provider>> {
        self.inner.fee_history_cache()
    }

    async fn suggested_priority_fee(&self) -> Result<U256, Self::Error> {
        LoadFee::suggested_priority_fee(&self.inner).await
    }
}

impl<N, Rpc> LoadState for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert<Primitives = N::Primitives>,
{
}

impl<N, Rpc> EthState for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert<Primitives = N::Primitives>,
{
    #[inline]
    fn max_proof_window(&self) -> u64 {
        self.inner.max_proof_window()
    }
}

impl<N, Rpc> EthFees for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    OpEthApiError: FromEvmError<N::Evm>,
    Rpc: RpcConvert<Primitives = N::Primitives, Error = OpEthApiError>,
{
}

impl<N, Rpc> Trace for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    OpEthApiError: FromEvmError<N::Evm>,
    Rpc: RpcConvert<Primitives = N::Primitives>,
{
}

impl<N, Rpc> AddDevSigners for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert<
        Network: RpcTypes<TransactionRequest: SignableTxRequest<ProviderTx<N::Provider>>>,
    >,
{
    fn with_dev_accounts(&self) {
        self.inner.with_dev_accounts()
    }
}

impl<N: RpcNodeCore, Rpc: RpcConvert> std::fmt::Debug for FlashblocksEthApi<N, Rpc> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlashblocksEthApi").finish_non_exhaustive()
    }
}

/// Builds [`OpEthApi`] for Optimism.
#[derive(Default, Debug)]
pub struct FlashblocksEthApiBuilder<NetworkT = Optimism> {
    inner: OpEthApiBuilder<NetworkT>,
}

// impl<NetworkT> Default for FlashblocksEthApiBuilder<NetworkT> {
//     fn default() -> Self {
//         Self {
//             sequencer_url: None,
//             sequencer_headers: Vec::new(),
//             min_suggested_priority_fee: 1_000_000,
//             _nt: PhantomData,
//         }
//     }
// }

impl<NetworkT> FlashblocksEthApiBuilder<NetworkT> {
    /// Creates a [`OpEthApiBuilder`] instance from core components.
    pub const fn new(inner: OpEthApiBuilder<NetworkT>) -> Self {
        Self { inner }
    }
}

impl<N, NetworkT> EthApiBuilder<N> for FlashblocksEthApiBuilder<NetworkT>
where
    N: FullNodeComponents<Evm: ConfigureEvm<NextBlockEnvCtx: BuildPendingEnv<HeaderTy<N::Types>>>>,
    NetworkT: RpcTypes + Default,
    OpRpcConvert<N, NetworkT>: RpcConvert<Network = NetworkT>,
    FlashblocksEthApi<N, OpRpcConvert<N, NetworkT>>:
        FullEthApiServer<Provider = N::Provider, Pool = N::Pool> + AddDevSigners,
{
    type EthApi = FlashblocksEthApi<N, OpRpcConvert<N, NetworkT>>;

    async fn build_eth_api(self, ctx: EthApiCtx<'_, N>) -> eyre::Result<Self::EthApi> {
        todo!()
        // let Self {
        //     sequencer_url,
        //     sequencer_headers,
        //     min_suggested_priority_fee,
        //     ..
        // } = self;
        // let rpc_converter =
        //     RpcConverter::new(OpReceiptConverter::new(ctx.components.provider().clone()))
        //         .with_mapper(OpTxInfoMapper::new(ctx.components.provider().clone()));
        //
        // let eth_api = ctx
        //     .eth_api_builder()
        //     .with_rpc_converter(rpc_converter)
        //     .build_inner();
        //
        // let sequencer_client = if let Some(url) = sequencer_url {
        //     Some(
        //         SequencerClient::new_with_headers(&url, sequencer_headers)
        //             .await
        //             .wrap_err_with(|| "Failed to init sequencer client with: {url}")?,
        //     )
        // } else {
        //     None
        // };
        //
        // Ok(OpEthApi::new(
        //     eth_api,
        //     sequencer_client,
        //     U256::from(min_suggested_priority_fee),
        // ))
    }
}
