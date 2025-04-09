use alloy_primitives::{map::foldhash::HashMap, Address, Bytes, U256};
use reth::builder::PayloadBuilderError;
use reth::{
    chainspec::EthChainSpec,
    payload::PayloadId,
    revm::{Database, State},
};
use reth::{
    revm::primitives::{alloy_primitives::Bloom, B256},
    rpc::types::Withdrawal,
};
use reth_evm::{execute::BlockBuilder, ConfigureEvm};
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::txpool::interop::MaybeInteropTransaction;
use reth_optimism_node::txpool::OpPooledTransaction;
use reth_optimism_node::OpNextBlockEnvAttributes;
use reth_optimism_payload_builder::builder::{ExecutionInfo, OpPayloadBuilderCtx};
use reth_optimism_payload_builder::payload::OpPayloadBuilderAttributes;
use reth_optimism_payload_builder::OpPayloadPrimitives;
use reth_payload_util::PayloadTransactions;
use reth_primitives::NodePrimitives;
use reth_primitives::{SealedHeader, TxTy};
use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction, TransactionPool};
use revm::context::BlockEnv;
use serde::{Deserialize, Serialize};
use serde_json::Value;
/// Represents the base configuration of an execution payload that remains constant
/// throughout block construction. This includes fundamental block properties like
/// parent hash, block number, and other header fields that are determined at
/// block creation and cannot be modified.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ExecutionPayloadBaseV1 {
    /// Ecotone parent beacon block root
    pub parent_beacon_block_root: B256,
    /// The parent hash of the block.
    pub parent_hash: B256,
    /// The fee recipient of the block.
    pub fee_recipient: Address,
    /// The previous randao of the block.
    pub prev_randao: B256,
    /// The block number.
    #[serde(with = "alloy_serde::quantity")]
    pub block_number: u64,
    /// The gas limit of the block.
    #[serde(with = "alloy_serde::quantity")]
    pub gas_limit: u64,
    /// The timestamp of the block.
    #[serde(with = "alloy_serde::quantity")]
    pub timestamp: u64,
    /// The extra data of the block.
    pub extra_data: Bytes,
    /// The base fee per gas of the block.
    pub base_fee_per_gas: U256,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct FlashblocksPayloadV1 {
    /// The payload id of the flashblock
    pub payload_id: PayloadId,
    /// The index of the flashblock in the block
    pub index: u64,
    /// The base execution payload configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<ExecutionPayloadBaseV1>,
    /// The delta/diff containing modified portions of the execution payload
    pub diff: ExecutionPayloadFlashblockDeltaV1,
    /// Additional metadata associated with the flashblock
    pub metadata: Value,
}

/// Represents the modified portions of an execution payload within a flashblock.
/// This structure contains only the fields that can be updated during block construction,
/// such as state root, receipts, logs, and new transactions. Other immutable block fields
/// like parent hash and block number are excluded since they remain constant throughout
/// the block's construction.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ExecutionPayloadFlashblockDeltaV1 {
    /// The state root of the block.
    pub state_root: B256,
    /// The receipts root of the block.
    pub receipts_root: B256,
    /// The logs bloom of the block.
    pub logs_bloom: Bloom,
    /// The gas used of the block.
    #[serde(with = "alloy_serde::quantity")]
    pub gas_used: u64,
    /// The block hash of the block.
    pub block_hash: B256,
    /// The transactions of the block.
    pub transactions: Vec<Bytes>,
    /// Array of [`Withdrawal`] enabled with V2
    pub withdrawals: Vec<Withdrawal>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FlashblocksMetadata<N: NodePrimitives> {
    pub receipts: HashMap<B256, N::Receipt>,
    pub new_account_balances: HashMap<Address, U256>,
    pub block_number: u64,
}

pub trait PayloadBuilderCtx<Evm, Chainspec>
where
    Evm: ConfigureEvm<Primitives: OpPayloadPrimitives, NextBlockEnvCtx = OpNextBlockEnvAttributes>,
    Chainspec: EthChainSpec + OpHardforks,
{
    fn parent(&self) -> &SealedHeader;

    fn attributes(&self) -> &OpPayloadBuilderAttributes<TxTy<Evm::Primitives>>;

    fn extra_data(&self) -> Result<Bytes, PayloadBuilderError>;

    fn best_transaction_attributes(&self, block_env: &BlockEnv) -> BestTransactionsAttributes;

    fn payload_id(&self) -> PayloadId;

    fn is_holocene_active(&self) -> bool;

    fn is_better_payload(&self, total_fees: U256) -> bool;

    fn block_builder<'a, DB>(
        &'a self,
        db: &'a mut State<DB>,
    ) -> Result<impl BlockBuilder<Primitives = Evm::Primitives> + 'a, PayloadBuilderError>
    where
        DB: Database,
        DB::Error: Send + Sync + 'static,
        DB: reth::revm::Database;

    fn execute_sequencer_transactions(
        &self,
        builder: &mut impl BlockBuilder<Primitives = Evm::Primitives>,
    ) -> Result<ExecutionInfo, PayloadBuilderError>;

    fn execute_best_transactions<Pool>(
        &self,
        info: &mut ExecutionInfo,
        builder: &mut impl BlockBuilder<Primitives = Evm::Primitives>,
        best_txs: impl PayloadTransactions<Transaction = TxTy<Evm::Primitives>>,
        gas_limit: u64,
        pool: &Pool,
    ) -> Result<Option<()>, PayloadBuilderError>
    where
        Pool: TransactionPool<Transaction = TxTy<Evm::Primitives>>;
}

impl<Evm, Chainspec> PayloadBuilderCtx<Evm, Chainspec> for OpPayloadBuilderCtx<Evm, Chainspec>
where
    // Cons: SignedTransaction + From<Pooled>,
    // Pooled: SignedTransaction + TryFrom<Cons, Error: core::error::Error>,
    Evm: ConfigureEvm<Primitives: OpPayloadPrimitives, NextBlockEnvCtx = OpNextBlockEnvAttributes>,
    Chainspec: EthChainSpec + OpHardforks,
{
    fn parent(&self) -> &SealedHeader {
        self.parent()
    }
    fn attributes(&self) -> &OpPayloadBuilderAttributes<TxTy<Evm::Primitives>> {
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
    ) -> Result<impl BlockBuilder<Primitives = Evm::Primitives> + 'a, PayloadBuilderError>
    where
        DB::Error: Send + Sync + 'static,
        DB: Database,
    {
        self.block_builder(db)
    }

    fn execute_sequencer_transactions(
        &self,
        builder: &mut impl BlockBuilder<Primitives = Evm::Primitives>,
    ) -> Result<ExecutionInfo, PayloadBuilderError> {
        self.execute_sequencer_transactions(builder)
    }

    fn execute_best_transactions<Pool>(
        &self,
        info: &mut ExecutionInfo,
        builder: &mut impl BlockBuilder<Primitives = Evm::Primitives>,
        best_txs: impl PayloadTransactions<Transaction = TxTy<Evm::Primitives>>,
        gas_limit: u64,
        pool: &Pool,
    ) -> Result<Option<()>, PayloadBuilderError>
    where
        Pool: TransactionPool<Transaction = TxTy<Evm::Primitives>>,
    {
        // self.execute_best_transactions(info, builder, best_txs)
        todo!()
    }
}
