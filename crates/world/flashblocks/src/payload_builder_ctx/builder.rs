use reth::revm::cancelled::CancelOnDrop;
use reth_basic_payload_builder::PayloadConfig;
use reth_optimism_node::{OpBuiltPayload, OpPayloadBuilderAttributes};
use reth_primitives::NodePrimitives;

use crate::builder::FlashblocksPayloadBuilder;

use super::PayloadBuilderCtx;

pub trait PaylodBuilderCtxBuilder<Evm, ChainSpec> {
    type PayloadBuilderCtx: PayloadBuilderCtx<Evm = Evm, ChainSpec = ChainSpec>;

    fn build<Pool, Client, Txs, N>(
        payload_builder: &FlashblocksPayloadBuilder<Pool, Client, Evm, Self, Txs>,
        config: PayloadConfig<OpPayloadBuilderAttributes<N::SignedTx>, N::BlockHeader>,
        cancel: CancelOnDrop,
        best_payload: Option<OpBuiltPayload<N>>,
    ) -> Self::PayloadBuilderCtx
    where
        Self: Sized,
        N: NodePrimitives;
}

