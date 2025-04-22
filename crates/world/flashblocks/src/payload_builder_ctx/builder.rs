use reth_basic_payload_builder::BuildArguments;
use reth_optimism_node::{OpBuiltPayload, OpPayloadBuilderAttributes};
use reth_primitives::NodePrimitives;

use crate::builder::FlashblocksPayloadBuilder;

use super::PayloadBuilderCtx;

pub trait PaylodBuilderCtxBuilder<Evm, ChainSpec> {
    type PayloadBuilderCtx: PayloadBuilderCtx<Evm = Evm, ChainSpec = ChainSpec>;

    fn build<Pool, Client, Txs, N>(
        payload_builder: &FlashblocksPayloadBuilder<Pool, Client, Evm, Self, Txs>,
        args: BuildArguments<OpPayloadBuilderAttributes<N::SignedTx>, OpBuiltPayload<N>>,
    ) -> Self::PayloadBuilderCtx
    where
        Self: Sized,
        N: NodePrimitives;
}

