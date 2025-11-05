use alloy_eips::eip4895::Withdrawals;
use alloy_primitives::U256;
use op_alloy_consensus::EIP1559ParamError;
use reth::{builder::PayloadBuilderError, payload::PayloadId, revm::State};
use reth_chainspec::EthereumHardforks;
use reth_evm::{
    block::BlockExecutor, execute::BlockBuilder, op_revm::OpSpecId, ConfigureEvm, Evm, EvmEnv,
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{
    txpool::{OpPooledTransaction, OpPooledTx},
    OpEvmConfig,
};
use reth_optimism_payload_builder::{
    builder::{ExecutionInfo, OpPayloadBuilderCtx},
    payload::OpPayloadBuilderAttributes,
};
use reth_payload_primitives::BuildNextEnv;
use reth_payload_util::PayloadTransactions;
use reth_primitives::{SealedHeader, TxTy};
use reth_provider::ChainSpecProvider;
use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction, TransactionPool};
use revm::context::BlockEnv;

use crate::traits::context_builder::PayloadBuilderCtxBuilder;

/// Context trait for building payloads with flashblock support.
///
/// This trait abstracts execution into a context that you have control over.
/// e.g. custom ordering, or execution policies may be implemented while retaining
/// the core flashblocks functionality in the [`FlashblocksPayloadBuilder`].
///
/// # Type Parameters
///
/// * `Evm` - EVM configuration implementing [`ConfigureEvm`]
/// * `ChainSpec` - Chain specification supporting Optimism hardforks
/// * `Transaction` - Pool transaction type with Optimism support
pub trait PayloadBuilderCtx: Send + Sync {
    /// EVM configuration type that handles EVM setup and execution.
    type Evm: ConfigureEvm;

    /// Chain specification type that supports Optimism hardforks.
    type ChainSpec: OpHardforks;

    /// The Pooled transaction type for this node.
    type Transaction: PoolTransaction<Consensus = TxTy<<Self::Evm as ConfigureEvm>::Primitives>>
        + OpPooledTx;

    /// Provides access to the EVM configuration used throughout payload building.
    ///
    /// This configuration determines how transactions are executed, what opcodes
    /// are available, and which hardfork rules apply during block construction.
    fn evm_config(&self) -> &Self::Evm;

    /// Constructs the execution environment that will be used for all transaction processing.
    ///
    /// Sets up block-level context (timestamp, difficulty, gas limit) and applies
    /// Optimism-specific configuration like fee parameters and L1 data availability costs.
    fn evm_env(&self) -> Result<EvmEnv<OpSpecId>, EIP1559ParamError>;

    /// Exposes the chain specification to determine active hardforks and network rules.
    ///
    /// Used to validate transactions against current network rules and enable/disable
    /// features based on block height or timestamp.
    fn spec(&self) -> &Self::ChainSpec;

    /// Provides the parent block header that this payload builds upon.
    ///
    /// The new block inherits context from this parent, including state root,
    /// block number (parent + 1), and chain continuity validation.
    fn parent(&self) -> &SealedHeader;

    /// Exposes the consensus layer's instructions for building this specific payload.
    ///
    /// Contains the target timestamp, fee recipient, gas limit, and other parameters
    /// that the consensus client has determined for this block slot.
    fn attributes(
        &self,
    ) -> &OpPayloadBuilderAttributes<TxTy<<Self::Evm as ConfigureEvm>::Primitives>>;

    /// Calculates transaction selection criteria based on current network conditions.
    ///
    /// Determines the minimum gas price, priority fee requirements, and other filters
    /// that transactions must meet to be considered for inclusion in this block.
    fn best_transaction_attributes(&self, block_env: &BlockEnv) -> BestTransactionsAttributes;

    /// Returns the unique identifier that tracks this specific payload building job.
    ///
    /// Used for logging, metrics, and coordinating between different components
    /// that may be working on the same payload concurrently.
    fn payload_id(&self) -> PayloadId;

    /// Evaluates whether a newly built payload should replace the current best candidate.
    ///
    /// Typically compares total fee revenue, but may include other factors like
    /// transaction count, MEV opportunities, or builder preferences.
    fn is_better_payload(&self, total_fees: U256) -> bool;

    /// Creates a block builder that will execute transactions and maintain block state.
    ///
    /// The builder handles transaction execution, state updates, receipt generation,
    /// and maintains all the data structures needed to finalize the block.
    fn block_builder<'a, DB>(
        &'a self,
        db: &'a mut State<DB>,
    ) -> Result<
        impl BlockBuilder<
                Executor: BlockExecutor<Evm: Evm<DB = &'a mut State<DB>, BlockEnv = BlockEnv>>,
                Primitives = <Self::Evm as ConfigureEvm>::Primitives,
            > + 'a,
        PayloadBuilderError,
    >
    where
        DB: reth_evm::Database + 'a,
        DB::Error: Send + Sync + 'static;

    /// Processes mandatory system transactions that must be included before user transactions.
    ///
    /// For Optimism, this typically includes L1 deposit transactions that represent
    /// funds being bridged from L1 to L2. These transactions cannot fail and must
    /// be processed in the exact order specified by the L1 chain.
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

    /// Processes user transactions from the mempool until `gas_limit` is reached.
    ///
    /// Returns `None` if the parent [`CancelOnDrop`] token was dropped by the [`PayloadJobsGenerator`] type.
    fn execute_best_transactions<'a, Pool, Txs, DB, Builder>(
        &self,
        pool: Pool,
        info: &mut ExecutionInfo,
        builder: &mut Builder,
        best_txs: Txs,
        gas_limit: u64,
    ) -> Result<Option<()>, PayloadBuilderError>
    where
        Pool: TransactionPool,
        DB: reth_evm::Database + 'a,
        DB::Error: Send + Sync + 'static,
        Builder: BlockBuilder<
            Primitives = <Self::Evm as ConfigureEvm>::Primitives,
            Executor: BlockExecutor<Evm: Evm<DB = &'a mut State<DB>, BlockEnv = BlockEnv>>,
        >,
        Txs: PayloadTransactions<Transaction = Self::Transaction>;

    /// Determines if validator withdrawals should be processed in this block.
    ///
    /// Checks if the Shanghai hardfork is active at the current timestamp, and
    /// if so, returns the withdrawals specified by the consensus layer. These
    /// represent validator stake withdrawals that must be processed automatically.
    fn withdrawals(&self) -> Option<&Withdrawals> {
        self.spec()
            .is_shanghai_active_at_timestamp(self.attributes().payload_attributes.timestamp)
            .then(|| &self.attributes().payload_attributes.withdrawals)
    }
}

#[derive(Debug, Default, Clone)]
pub struct OpPayloadBuilderCtxBuilder;

impl<Provider> PayloadBuilderCtxBuilder<Provider, OpEvmConfig, OpChainSpec>
    for OpPayloadBuilderCtxBuilder
where
    Provider: ChainSpecProvider<ChainSpec = OpChainSpec>,
{
    type PayloadBuilderCtx = OpPayloadBuilderCtx<OpEvmConfig, OpChainSpec>;

    fn build(
        &self,
        provider: Provider,
        evm: OpEvmConfig,
        da_config: reth_optimism_node::OpDAConfig,
        config: reth_basic_payload_builder::PayloadConfig<
            OpPayloadBuilderAttributes<op_alloy_consensus::OpTxEnvelope>,
            <<OpEvmConfig as ConfigureEvm>::Primitives as reth_node_api::NodePrimitives>::BlockHeader,
        >,
        cancel: &reth::revm::cancelled::CancelOnDrop,
        best_payload: Option<reth_optimism_node::OpBuiltPayload>,
    ) -> Self::PayloadBuilderCtx
    where
        Self: Sized,
    {
        OpPayloadBuilderCtx {
            evm_config: evm,
            da_config,
            chain_spec: provider.chain_spec(),
            config,
            cancel: cancel.clone(),
            best_payload,
        }
    }
}

impl PayloadBuilderCtx for OpPayloadBuilderCtx<OpEvmConfig, OpChainSpec> {
    type Evm = OpEvmConfig;
    type ChainSpec = OpChainSpec;
    type Transaction = OpPooledTransaction;

    fn evm_config(&self) -> &Self::Evm {
        &self.evm_config
    }

    fn spec(&self) -> &Self::ChainSpec {
        self.chain_spec.as_ref()
    }

    fn evm_env(&self) -> Result<EvmEnv<OpSpecId>, EIP1559ParamError> {
        self.evm_config.evm_env(self.parent())
    }

    fn parent(&self) -> &SealedHeader {
        self.parent()
    }

    fn attributes(
        &self,
    ) -> &OpPayloadBuilderAttributes<TxTy<<Self::Evm as ConfigureEvm>::Primitives>> {
        self.attributes()
    }

    fn best_transaction_attributes(
        &self,
        block_env: &revm::context::BlockEnv,
    ) -> BestTransactionsAttributes {
        self.best_transaction_attributes(block_env)
    }

    fn payload_id(&self) -> PayloadId {
        self.payload_id()
    }

    fn is_better_payload(&self, total_fees: U256) -> bool {
        self.is_better_payload(total_fees)
    }

    /// Processes user transactions from the mempool until `gas_limit` is reached.
    ///
    /// Returns `None` if the parent [`CancelOnDrop`] token was dropped by the [`PayloadJobsGenerator`] type.
    fn execute_best_transactions<'a, Pool, Txs, DB, Builder>(
        &self,
        _pool: Pool,
        info: &mut ExecutionInfo,
        builder: &mut Builder,
        best_txs: Txs,
        _gas_limit: u64,
    ) -> Result<Option<()>, PayloadBuilderError>
    where
        Pool: TransactionPool,
        DB: reth_evm::Database + 'a,
        DB::Error: Send + Sync + 'static,
        Builder: BlockBuilder<
            Primitives = <Self::Evm as ConfigureEvm>::Primitives,
            Executor: BlockExecutor<Evm: Evm<DB = &'a mut State<DB>>>,
        >,
        Txs: PayloadTransactions<Transaction = Self::Transaction>,
    {
        self.execute_best_transactions(info, builder, best_txs)
    }

    /// Determines if validator withdrawals should be processed in this block.
    ///
    /// Checks if the Shanghai hardfork is active at the current timestamp, and
    /// if so, returns the withdrawals specified by the consensus layer. These
    /// represent validator stake withdrawals that must be processed automatically.
    fn withdrawals(&self) -> Option<&Withdrawals> {
        self.spec()
            .is_shanghai_active_at_timestamp(self.attributes().payload_attributes.timestamp)
            .then(|| &self.attributes().payload_attributes.withdrawals)
    }

    fn block_builder<'a, DB>(
        &'a self,
        db: &'a mut State<DB>,
    ) -> Result<
        impl BlockBuilder<
                Primitives = <Self::Evm as ConfigureEvm>::Primitives,
                Executor: BlockExecutor<Evm: Evm<DB = &'a mut State<DB>, BlockEnv = BlockEnv>>,
            > + 'a,
        PayloadBuilderError,
    >
    where
        DB: reth_evm::Database + 'a,
        DB::Error: Send + Sync + 'static,
    {
        {
            // Requires a manual implementation here because upstream
            // opaque types don't have sufficient functionality
            let this = &self;
            let parent = this.parent();
            let attributes = <OpEvmConfig as ConfigureEvm>::NextBlockEnvCtx::build_next_env(
                this.attributes(),
                this.parent(),
                this.chain_spec.as_ref(),
            )
            .map_err(PayloadBuilderError::other)?;

            let evm_env = this
                .evm_config
                .next_evm_env(parent, &attributes)
                .map_err(PayloadBuilderError::other)?;

            let evm = this.evm_config.evm_with_env(db, evm_env);
            let ctx = this
                .evm_config
                .context_for_next_block(parent, attributes)
                .map_err(PayloadBuilderError::other)?;

            Ok(this.evm_config.create_block_builder(evm, parent, ctx))
        }
    }

    fn execute_sequencer_transactions<'a, DB>(
        &self,
        builder: &mut impl BlockBuilder<
            Primitives = <Self::Evm as ConfigureEvm>::Primitives,
            Executor: BlockExecutor<Evm: Evm<DB = &'a mut State<DB>>>,
        >,
    ) -> Result<ExecutionInfo, PayloadBuilderError>
    where
        DB: reth_evm::Database + 'a,
        DB::Error: Send + Sync + 'static,
    {
        self.execute_sequencer_transactions(builder)
    }
}
