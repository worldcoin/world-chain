use std::sync::Arc;

use op_alloy_consensus::OpTxEnvelope;
use reth::revm::cancelled::CancelOnDrop;
use reth_basic_payload_builder::PayloadConfig;
use reth_evm::ConfigureEvm;
use reth_optimism_node::{OpBuiltPayload, OpEvmConfig, OpPayloadBuilderAttributes};
use reth_optimism_payload_builder::config::OpDAConfig;
use reth_primitives::NodePrimitives;

use super::PayloadBuilderCtx;

pub trait PayloadBuilderCtxBuilder<EvmConfig: ConfigureEvm, ChainSpec, Transaction>: Clone + Send + Sync {
    type PayloadBuilderCtx: PayloadBuilderCtx<
        Evm = EvmConfig,
        ChainSpec = ChainSpec,
        Transaction = Transaction,
    >;

    fn build<Txs>(
        &self,
        evm: EvmConfig,
        da_config: OpDAConfig,
        chain_spec: Arc<ChainSpec>,
        config: PayloadConfig<
            OpPayloadBuilderAttributes<OpTxEnvelope>,
            <<OpEvmConfig as ConfigureEvm>::Primitives as NodePrimitives>::BlockHeader,
        >,
        cancel: &CancelOnDrop,
        best_payload: Option<OpBuiltPayload>,
    ) -> Self::PayloadBuilderCtx
    where
        Self: Sized;
}
