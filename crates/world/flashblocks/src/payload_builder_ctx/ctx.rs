use alloy_eips::eip4895::Withdrawals;
use alloy_primitives::U256;
use reth::builder::PayloadBuilderError;
use reth::payload::PayloadId;
use reth::revm::State;
use reth_chainspec::EthereumHardforks;
use reth_evm::block::BlockExecutor;
use reth_evm::op_revm::OpSpecId;
use reth_evm::{execute::BlockBuilder, ConfigureEvm};
use reth_evm::{Evm, EvmEnv};
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::txpool::OpPooledTx;
use reth_optimism_payload_builder::builder::ExecutionInfo;
use reth_optimism_payload_builder::payload::OpPayloadBuilderAttributes;
use reth_payload_util::PayloadTransactions;
use reth_primitives::{SealedHeader, TxTy};
use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction};
use revm::context::BlockEnv;

pub trait PayloadBuilderCtx: Send + Sync {
    type Evm: ConfigureEvm;
    type ChainSpec: OpHardforks;
    type Transaction: PoolTransaction<Consensus = TxTy<<Self::Evm as ConfigureEvm>::Primitives>>
        + OpPooledTx;

    fn evm_config(&self) -> &Self::Evm;

    fn evm_env(&self) -> EvmEnv<OpSpecId>;

    fn spec(&self) -> &Self::ChainSpec;

    fn parent(&self) -> &SealedHeader;

    fn attributes(
        &self,
    ) -> &OpPayloadBuilderAttributes<TxTy<<Self::Evm as ConfigureEvm>::Primitives>>;

    fn best_transaction_attributes(&self, block_env: &BlockEnv) -> BestTransactionsAttributes;

    fn payload_id(&self) -> PayloadId;

    fn is_better_payload(&self, total_fees: U256) -> bool;

    fn block_builder<'a, DB>(
        &'a self,
        db: &'a mut State<DB>,
    ) -> Result<
        impl BlockBuilder<
                Executor: BlockExecutor<Evm: Evm<DB = &'a mut State<DB>>>,
                Primitives = <Self::Evm as ConfigureEvm>::Primitives,
            > + 'a,
        PayloadBuilderError,
    >
    where
        DB: reth_evm::Database + 'a,
        DB::Error: Send + Sync + 'static;

    fn execute_sequencer_transactions<'a, DB>(
        &self,
        builder: &mut impl BlockBuilder<
            Primitives = <Self::Evm as ConfigureEvm>::Primitives,
            Executor: BlockExecutor<Evm: Evm<DB = &'a mut State<DB>>>,
        >,
    ) -> Result<ExecutionInfo, PayloadBuilderError>
    where
        DB: reth_evm::Database + 'a,
        DB::Error: Send + Sync + 'static;

    fn execute_best_transactions<'a, Txs, DB, Builder>(
        &self,
        info: &mut ExecutionInfo,
        builder: &mut Builder,
        best_txs: Txs,
        gas_limit: u64,
    ) -> Result<Option<()>, PayloadBuilderError>
    where
        DB: reth_evm::Database + 'a,
        DB::Error: Send + Sync + 'static,
        Builder: BlockBuilder<
            Primitives = <Self::Evm as ConfigureEvm>::Primitives,
            Executor: BlockExecutor<Evm: Evm<DB = &'a mut State<DB>>>,
        >,
        Txs: PayloadTransactions<Transaction = Self::Transaction>;

    fn withdrawals(&self) -> Option<&Withdrawals> {
        self.spec()
            .is_shanghai_active_at_timestamp(self.attributes().payload_attributes.timestamp)
            .then(|| &self.attributes().payload_attributes.withdrawals)
    }
}
