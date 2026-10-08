use op_alloy_rpc_types::OpTransactionReceipt;
use reth_chainspec::ChainSpecProvider;
use reth_optimism_forks::OpHardforks;
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::{OpEthApi, OpEthApiError};
use reth_rpc_eth_api::{
    EthApiTypes, RpcConvert, RpcNodeCore, RpcTypes,
    helpers::{LoadPendingBlock, LoadReceipt},
};

use crate::eth::FlashblocksEthApi;

impl<N, Rpc> LoadReceipt for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Primitives = OpPrimitives, Provider: ChainSpecProvider<ChainSpec: OpHardforks>>,
    Rpc: RpcConvert<Primitives = N::Primitives, Error = Self::Error> + Clone,
    OpEthApi<N, Rpc>: LoadReceipt + Clone,
    Self: LoadPendingBlock
        + EthApiTypes<
            RpcConvert: RpcConvert<
                Primitives = Self::Primitives,
                Error = Self::Error,
                Network = Self::NetworkTypes,
            >,
            NetworkTypes: RpcTypes<Receipt = OpTransactionReceipt>,
            Error = OpEthApiError,
        > + RpcNodeCore<Primitives = OpPrimitives, Provider: ChainSpecProvider<ChainSpec: OpHardforks>>,
{
}
