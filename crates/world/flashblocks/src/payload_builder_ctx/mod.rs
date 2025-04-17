use alloy_eips::eip4895::Withdrawals;
use alloy_primitives::{Bytes, U256};
use reth::builder::PayloadBuilderError;
use reth::{
    chainspec::EthChainSpec,
    payload::PayloadId,
    revm::{Database, State},
};
use reth_chainspec::EthereumHardforks;
use reth_evm::block::BlockExecutor;
use reth_evm::{execute::BlockBuilder, ConfigureEvm};
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::txpool::interop::MaybeInteropTransaction;
use reth_optimism_payload_builder::builder::ExecutionInfo;
use reth_optimism_payload_builder::payload::OpPayloadBuilderAttributes;
use reth_payload_util::PayloadTransactions;
use reth_primitives::{SealedHeader, TxTy};
use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction, TransactionPool};
use revm::context::BlockEnv;

mod op;

pub trait PayloadBuilderCtx {
    type Evm: ConfigureEvm;
    type ChainSpec: OpHardforks + EthChainSpec;

    fn evm(&self) -> &Self::Evm;

    fn evm_mut(&mut self) -> &mut Self::Evm;

    fn spec(&self) -> &Self::ChainSpec;

    fn parent(&self) -> &SealedHeader;

    fn attributes(
        &self,
    ) -> &OpPayloadBuilderAttributes<TxTy<<Self::Evm as ConfigureEvm>::Primitives>>;

    fn extra_data(&self) -> Result<Bytes, PayloadBuilderError>;

    fn best_transaction_attributes(&self, block_env: &BlockEnv) -> BestTransactionsAttributes;

    fn payload_id(&self) -> PayloadId;

    fn is_holocene_active(&self) -> bool;

    fn is_better_payload(&self, total_fees: U256) -> bool;

    fn block_builder<'a, DB>(
        &'a self,
        db: &'a mut State<DB>,
    ) -> Result<
        impl BlockBuilder<Primitives = <Self::Evm as ConfigureEvm>::Primitives> + 'a,
        PayloadBuilderError,
    >
    where
        DB: Database,
        DB::Error: Send + Sync + 'static,
        DB: reth::revm::Database;

    fn execute_sequencer_transactions(
        &self,
        builder: &mut impl BlockBuilder<Primitives = <Self::Evm as ConfigureEvm>::Primitives>,
    ) -> Result<ExecutionInfo, PayloadBuilderError>;

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
        >;

    fn withdrawals(&self) -> Option<&Withdrawals> {
        self.spec()
            .is_shanghai_active_at_timestamp(self.attributes().payload_attributes.timestamp)
            .then(|| &self.attributes().payload_attributes.withdrawals)
    }
}
