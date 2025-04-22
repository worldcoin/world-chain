use reth::revm::cancelled::CancelOnDrop;
use reth_basic_payload_builder::PayloadConfig;
use reth_evm::ConfigureEvm;
use reth_optimism_node::{OpBuiltPayload, OpPayloadBuilderAttributes};
use reth_primitives::NodePrimitives;

use crate::builder::FlashblocksPayloadBuilder;

use super::PayloadBuilderCtx;

pub trait PayloadBuilderCtxBuilder<Evm, ChainSpec>: Send + Sync
where
    Evm: ConfigureEvm,
{
    type PayloadBuilder;
    type PayloadBuilderCtx: PayloadBuilderCtx<Evm = Evm, ChainSpec = ChainSpec>;

    fn build<Txs>(
        payload_builder: &Self::PayloadBuilder,
        config: PayloadConfig<
            OpPayloadBuilderAttributes<<Evm::Primitives as NodePrimitives>::SignedTx>,
            <Evm::Primitives as NodePrimitives>::BlockHeader,
        >,
        cancel: CancelOnDrop,
        best_payload: Option<OpBuiltPayload<Evm::Primitives>>,
    ) -> Self::PayloadBuilderCtx
    where
        Self: Sized,
}

