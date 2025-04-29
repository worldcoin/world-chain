use std::sync::Arc;

use reth::revm::cancelled::CancelOnDrop;
use reth_basic_payload_builder::PayloadConfig;
use reth_evm::ConfigureEvm;
use reth_optimism_node::{OpBuiltPayload, OpPayloadBuilderAttributes};
use reth_optimism_payload_builder::config::OpDAConfig;
use reth_primitives::NodePrimitives;

use super::PayloadBuilderCtx;

pub trait PayloadBuilderCtxBuilder<Evm, ChainSpec, Transaction>: Clone + Send + Sync
where
    Evm: ConfigureEvm,
{
    type PayloadBuilderCtx: PayloadBuilderCtx<
        Evm = Evm,
        ChainSpec = ChainSpec,
        Transaction = Transaction,
    >;

    fn build<Txs>(
        &self,
        evm: Evm,
        da_config: OpDAConfig,
        chain_spec: Arc<ChainSpec>,
        config: PayloadConfig<
            OpPayloadBuilderAttributes<<Evm::Primitives as NodePrimitives>::SignedTx>,
            <Evm::Primitives as NodePrimitives>::BlockHeader,
        >,
        cancel: CancelOnDrop,
        best_payload: Option<OpBuiltPayload<Evm::Primitives>>,
    ) -> Self::PayloadBuilderCtx
    where
        Self: Sized;
}
