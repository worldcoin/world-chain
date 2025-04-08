use alloy_primitives::{Bytes, U256};
use reth::{
    api::PayloadBuilderError,
    chainspec::EthChainSpec,
    payload::PayloadId,
    revm::{Database, State},
};
use reth_evm::{execute::BlockBuilder, ConfigureEvm};
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{txpool::interop::MaybeInteropTransaction, OpNextBlockEnvAttributes};
use reth_optimism_payload_builder::payload::OpPayloadBuilderAttributes;
use reth_optimism_payload_builder::{
    builder::{ExecutionInfo, OpPayloadBuilderCtx},
    OpPayloadPrimitives,
};
use reth_payload_util::PayloadTransactions;
use reth_primitives::{SealedHeader, TxTy};
use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction};
use revm::context::BlockEnv;

use super::PayloadBuilderCtx;

impl<Evm, Chainspec> PayloadBuilderCtx for OpPayloadBuilderCtx<Evm, Chainspec>
where
    Evm: ConfigureEvm<Primitives: OpPayloadPrimitives, NextBlockEnvCtx = OpNextBlockEnvAttributes>,
    Chainspec: EthChainSpec + OpHardforks,
{
    type Evm = Evm;
    type ChainSpec = Chainspec;

    fn parent(&self) -> &SealedHeader {
        self.parent()
    }

    fn attributes(
        &self,
    ) -> &OpPayloadBuilderAttributes<TxTy<<Self::Evm as ConfigureEvm>::Primitives>> {
        self.attributes()
    }

    fn extra_data(&self) -> Result<Bytes, PayloadBuilderError> {
        self.extra_data()
    }

    fn best_transaction_attributes(&self, block_env: &BlockEnv) -> BestTransactionsAttributes {
        self.best_transaction_attributes(block_env)
    }

    fn payload_id(&self) -> PayloadId {
        self.payload_id()
    }

    fn is_holocene_active(&self) -> bool {
        self.is_holocene_active()
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

    fn execute_best_transactions(
        &self,
        info: &mut ExecutionInfo,
        builder: &mut impl BlockBuilder<Primitives = Evm::Primitives>,
        best_txs: impl PayloadTransactions<
            Transaction: PoolTransaction<Consensus = TxTy<Evm::Primitives>>
                             + MaybeInteropTransaction,
        >,
        _gas_limit: u64,
    ) -> Result<Option<()>, PayloadBuilderError> {
        self.execute_best_transactions(info, builder, best_txs)
    }
}
