use alloy_eips::eip4895::Withdrawals;
use alloy_op_evm::OpEvm;
use alloy_primitives::U256;
use reth::builder::PayloadBuilderError;
use reth::payload::PayloadId;
use reth::revm::State;
use reth_chainspec::EthereumHardforks;
use reth_evm::block::BlockExecutor;
use reth_evm::op_revm::OpSpecId;
use reth_evm::precompiles::PrecompilesMap;
use reth_evm::{execute::BlockBuilder, ConfigureEvm};
use reth_evm::{Evm, EvmEnv};
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::txpool::OpPooledTx;
use reth_optimism_payload_builder::payload::OpPayloadBuilderAttributes;
use reth_optimism_primitives::OpPrimitives;
use reth_payload_util::PayloadTransactions;
use reth_primitives::{SealedHeader, TxTy};
use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction};
use revm::context::BlockEnv;
use revm::inspector::NoOpInspector;
use std::fmt::Debug;

use crate::builder::ExecutionInfo;
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
            Primitives = OpPrimitives,
            Executor: BlockExecutor<Evm: Evm<DB = &'a mut State<DB>>>,
        >,
        PayloadBuilderError,
    >
    where
        DB: reth_evm::Database + 'a,
        DB::Error: Send + Sync + 'static;

    fn execute_sequencer_transactions<DB, E: Default + Debug>(
        &self,
        db: &mut State<DB>,
    ) -> Result<ExecutionInfo<E>, PayloadBuilderError>
    where
        DB: revm::Database + Debug,
        DB::Error: Send + Sync + 'static;

    fn execute_best_transactions<'a, DB, E: Debug + Default, Txs>(
        &self,
        info: &mut ExecutionInfo<E>,
        builder: OpEvm<&'a mut State<DB>, NoOpInspector, PrecompilesMap>,
        best_txs: Txs,
        gas_limit: u64,
    ) -> Result<Option<()>, PayloadBuilderError>
    where
        DB: revm::Database + Debug + 'a,
        Txs: PayloadTransactions<Transaction = Self::Transaction>,
        DB::Error: Send + Sync + 'static;

    fn withdrawals(&self) -> Option<&Withdrawals> {
        self.spec()
            .is_shanghai_active_at_timestamp(self.attributes().payload_attributes.timestamp)
            .then(|| &self.attributes().payload_attributes.withdrawals)
    }
}
