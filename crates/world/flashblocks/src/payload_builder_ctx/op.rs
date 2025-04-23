use std::sync::Arc;

use alloy_primitives::U256;
use reth::builder::PayloadBuilderError;
use reth::revm::cancelled::CancelOnDrop;
use reth::{
    chainspec::EthChainSpec,
    payload::PayloadId,
    revm::{Database, State},
};
use reth_basic_payload_builder::PayloadConfig;
use reth_evm::block::BlockExecutor;
use reth_evm::{execute::BlockBuilder, ConfigureEvm};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::txpool::interop::MaybeInteropTransaction;
use reth_optimism_node::{OpBuiltPayload, OpEvmConfig, OpNextBlockEnvAttributes};
use reth_optimism_payload_builder::builder::{ExecutionInfo, OpPayloadBuilderCtx};
use reth_optimism_payload_builder::config::OpDAConfig;
use reth_optimism_payload_builder::payload::OpPayloadBuilderAttributes;
use reth_optimism_payload_builder::OpPayloadPrimitives;
use reth_payload_util::PayloadTransactions;
use reth_primitives::{NodePrimitives, SealedHeader, TxTy};
use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction};
use revm::context::BlockEnv;

use super::{PayloadBuilderCtx, PayloadBuilderCtxBuilder};

#[derive(Debug, Clone, Copy)]
pub struct OpPayloadBuilderCtxBuilder;

impl PayloadBuilderCtxBuilder<OpEvmConfig, OpChainSpec> for OpPayloadBuilderCtxBuilder {
    type PayloadBuilderCtx = OpPayloadBuilderCtx<OpEvmConfig, OpChainSpec>;

    fn build<Txs>(
        &self,
        evm_config: OpEvmConfig,
        da_config: OpDAConfig,
        chain_spec: Arc<OpChainSpec>,
        config: PayloadConfig<
            OpPayloadBuilderAttributes<
                <<OpEvmConfig as ConfigureEvm>::Primitives as NodePrimitives>::SignedTx,
            >,
            <<OpEvmConfig as ConfigureEvm>::Primitives as NodePrimitives>::BlockHeader,
        >,
        cancel: CancelOnDrop,
        best_payload: Option<OpBuiltPayload<<OpEvmConfig as ConfigureEvm>::Primitives>>,
    ) -> Self::PayloadBuilderCtx
    where
        Self: Sized,
    {
        OpPayloadBuilderCtx {
            evm_config,
            da_config,
            chain_spec,
            config,
            cancel,
            best_payload,
        }
    }
}

impl<Evm, Chainspec> PayloadBuilderCtx for OpPayloadBuilderCtx<Evm, Chainspec>
where
    Evm: ConfigureEvm<Primitives: OpPayloadPrimitives, NextBlockEnvCtx = OpNextBlockEnvAttributes>,
    Chainspec: EthChainSpec + OpHardforks,
{
    type Evm = Evm;
    type ChainSpec = Chainspec;

    fn spec(&self) -> &Self::ChainSpec {
        &self.chain_spec
    }

    fn parent(&self) -> &SealedHeader {
        self.parent()
    }

    fn attributes(
        &self,
    ) -> &OpPayloadBuilderAttributes<TxTy<<Self::Evm as ConfigureEvm>::Primitives>> {
        self.attributes()
    }

    fn best_transaction_attributes(&self, block_env: &BlockEnv) -> BestTransactionsAttributes {
        self.best_transaction_attributes(block_env)
    }

    fn payload_id(&self) -> PayloadId {
        self.payload_id()
    }

    fn is_better_payload(&self, total_fees: U256) -> bool {
        self.is_better_payload(total_fees)
    }

    fn block_builder<'a, DB>(
        &'a self,
        db: &'a mut State<DB>,
    ) -> Result<
        impl BlockBuilder<Primitives = <Self::Evm as ConfigureEvm>::Primitives> + 'a,
        PayloadBuilderError,
    >
    where
        DB::Error: Send + Sync + 'static,
        DB: Database,
    {
        self.block_builder(db)
    }

    fn execute_sequencer_transactions(
        &self,
        builder: &mut impl BlockBuilder<Primitives = <Self::Evm as ConfigureEvm>::Primitives>,
    ) -> Result<ExecutionInfo, PayloadBuilderError> {
        self.execute_sequencer_transactions(builder)
    }

    fn execute_best_transactions<Builder, Txs>(
        &self,
        info: &mut ExecutionInfo,
        builder: &mut Builder,
        best_txs: Txs,
        _gas_limit: u64,
    ) -> Result<Option<()>, PayloadBuilderError>
    where
        Txs: PayloadTransactions<
            Transaction: PoolTransaction<
                Consensus = TxTy<<Self::Evm as ConfigureEvm>::Primitives>,
            > + MaybeInteropTransaction,
        >,
        Builder: BlockBuilder<
            Primitives = <Self::Evm as ConfigureEvm>::Primitives,
            Executor: BlockExecutor,
        >,
    {
        self.execute_best_transactions(info, builder, best_txs)
    }
}
