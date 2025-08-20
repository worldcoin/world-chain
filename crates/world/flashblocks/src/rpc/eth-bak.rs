use reth::{
    api::{FullNodeComponents, HeaderTy},
    builder::rpc::{EthApiBuilder, EthApiCtx},
    rpc::{
        api::eth::{
            helpers::{pending_block::BuildPendingEnv, AddDevSigners, EthCall},
            FromEvmError,
        },
        compat::{RpcConvert, RpcTypes},
        eth::{FullEthApiServer, RpcNodeCore},
    },
};
use reth_evm::{ConfigureEvm, TxEnvFor};
use reth_optimism_rpc::{eth::OpRpcConvert, OpEthApi, OpEthApiBuilder, OpEthApiError};

#[derive(Clone, Debug)]
pub struct FlashblocksEthApi<N: RpcNodeCore, Rpc: RpcConvert> {
    inner: OpEthApi<N, Rpc>,
}

impl<N: RpcNodeCore, Rpc: RpcConvert<Primitives = N::Primitives>> FlashblocksEthApi<N, Rpc> {
    pub fn new(inner: OpEthApi<N, Rpc>) -> Self {
        Self { inner }
    }

    pub fn val(&self) {
        let x = self.inner.provider();
    }
}

#[derive(Debug, Default)]
pub struct FlashblocksEthApiBuilder<NetworkT> {
    inner: OpEthApiBuilder<NetworkT>,
}

impl<N, NetworkT> EthApiBuilder<N> for FlashblocksEthApiBuilder<NetworkT>
where
    N: FullNodeComponents<Evm: ConfigureEvm<NextBlockEnvCtx: BuildPendingEnv<HeaderTy<N::Types>>>>,
    NetworkT: RpcTypes + Default,
    OpRpcConvert<N, NetworkT>: RpcConvert<Network = NetworkT>,
    OpEthApi<N, OpRpcConvert<N, NetworkT>>:
        FullEthApiServer<Provider = N::Provider, Pool = N::Pool> + AddDevSigners,
    FlashblocksEthApi<N, OpRpcConvert<N, NetworkT>>:
        FullEthApiServer<Provider = N::Provider, Pool = N::Pool> + AddDevSigners,
{
    type EthApi = FlashblocksEthApi<N, OpRpcConvert<N, NetworkT>>;

    async fn build_eth_api(self, ctx: EthApiCtx<'_, N>) -> eyre::Result<Self::EthApi> {
        let inner = self.inner.build_eth_api(ctx).await?;
        Ok(FlashblocksEthApi { inner })
    }
}

// impl<N, NetworkT> FullNodeComponents for FlashblocksEthApi<N, OpRpcConvert<N, NetworkT>>
// where
//     N: FullNodeComponents + RpcNodeCore + std::fmt::Debug,
//     OpRpcConvert<N, NetworkT>: RpcConvert<Primitives = N::Primitives>,
// {
//     type Pool = OpEthApi<N, OpRpcConvert<N, NetworkT>>::Pool;
//
//     type Evm;
//
//     type Consensus;
//
//     type Network;
//
//     fn pool(&self) -> &Self::Pool {
//         todo!()
//     }
//
//     fn evm_config(&self) -> &Self::Evm {
//         todo!()
//     }
//
//     fn consensus(&self) -> &Self::Consensus {
//         todo!()
//     }
//
//     fn network(&self) -> &Self::Network {
//         todo!()
//     }
//
//     fn payload_builder_handle(
//         &self,
//     ) -> &reth::payload::PayloadBuilderHandle<<Self::Types as reth::api::NodeTypes>::Payload> {
//         todo!()
//     }
//
//     fn provider(&self) -> &Self::Provider {
//         todo!()
//     }
//
//     fn task_executor(&self) -> &reth::tasks::TaskExecutor {
//         todo!()
//     }
// }

impl<N, Rpc> EthCall for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    OpEthApiError: FromEvmError<N::Evm>,
    Rpc: RpcConvert<Primitives = N::Primitives, Error = OpEthApiError, TxEnv = TxEnvFor<N::Evm>>,
{
}
