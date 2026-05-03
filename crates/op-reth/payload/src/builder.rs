//! Minimal Optimism payload support types.

use crate::{
    OpAttributes, OpPayloadAttrs, OpPayloadBuilderAttributes, OpPayloadPrimitives,
    config::OpBuilderConfig, error::OpPayloadBuilderError, payload::OpBuiltPayload,
};
use alloy_consensus::{Transaction, Typed2718};
use alloy_evm::Evm as AlloyEvm;
use alloy_primitives::{B256, U256};
use alloy_rpc_types_debug::ExecutionWitness;
use alloy_rpc_types_engine::PayloadId;
use op_revm::L1BlockInfo;
use reth_basic_payload_builder::{
    BuildArguments, BuildOutcome, MissingPayloadBehaviour, PayloadBuilder, PayloadConfig,
    is_better_payload,
};
use reth_chainspec::EthChainSpec;
use reth_evm::{
    ConfigureEvm, Database,
    block::BlockExecutorFor,
    execute::{BlockBuilder, BlockExecutionError, BlockExecutor, BlockValidationError},
};
use reth_optimism_forks::OpHardforks;
use reth_optimism_primitives::transaction::OpTransaction;
use reth_optimism_txpool::{
    OpPooledTx,
    estimated_da_size::DataAvailabilitySized,
    interop::{MaybeInteropTransaction, is_valid_interop},
};
use reth_payload_builder_primitives::PayloadBuilderError;
use reth_payload_primitives::BuildNextEnv;
use reth_payload_util::{BestPayloadTransactions, PayloadTransactions};
use reth_primitives_traits::{
    HeaderTy, NodePrimitives, SealedHeader, SealedHeaderFor, SignedTransaction, TxTy,
};
use reth_revm::{cancelled::CancelOnDrop, db::State};
use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction, TransactionPool};
use revm::context::{Block, BlockEnv};
use std::{marker::PhantomData, sync::Arc};
use tracing::trace;

/// Compatibility shell for the upstream OP payload builder.
///
/// World Chain uses its standalone Flashblocks builder for block construction. This type remains
/// only so the unused upstream OP node wiring can type-check without vendoring the upstream builder
/// implementation.
#[derive(Debug)]
pub struct OpPayloadBuilder<
    Pool,
    Client,
    Evm,
    Txs = (),
    Attrs = OpPayloadBuilderAttributes<TxTy<<Evm as ConfigureEvm>::Primitives>>,
> {
    /// The rollup's compute pending block configuration option.
    pub compute_pending_block: bool,
    /// The type responsible for creating the evm.
    pub evm_config: Evm,
    /// Transaction pool.
    pub pool: Pool,
    /// Node client.
    pub client: Client,
    /// Settings for the builder, e.g. DA settings.
    pub config: OpBuilderConfig,
    /// The type responsible for yielding the best transactions for the payload if mempool
    /// transactions are allowed.
    pub best_transactions: Txs,
    /// Marker for the payload attributes type.
    _pd: PhantomData<Attrs>,
}

impl<Pool, Client, Evm, Txs, Attrs> Clone for OpPayloadBuilder<Pool, Client, Evm, Txs, Attrs>
where
    Pool: Clone,
    Client: Clone,
    Evm: ConfigureEvm,
    Txs: Clone,
{
    fn clone(&self) -> Self {
        Self {
            evm_config: self.evm_config.clone(),
            pool: self.pool.clone(),
            client: self.client.clone(),
            config: self.config.clone(),
            best_transactions: self.best_transactions.clone(),
            compute_pending_block: self.compute_pending_block,
            _pd: PhantomData,
        }
    }
}

impl<Pool, Client, Evm, Attrs> OpPayloadBuilder<Pool, Client, Evm, (), Attrs> {
    /// `OpPayloadBuilder` constructor.
    pub fn new(pool: Pool, client: Client, evm_config: Evm) -> Self {
        Self::with_builder_config(pool, client, evm_config, Default::default())
    }

    /// Configures the builder with the given [`OpBuilderConfig`].
    pub const fn with_builder_config(
        pool: Pool,
        client: Client,
        evm_config: Evm,
        config: OpBuilderConfig,
    ) -> Self {
        Self {
            pool,
            client,
            compute_pending_block: true,
            evm_config,
            config,
            best_transactions: (),
            _pd: PhantomData,
        }
    }
}

impl<Pool, Client, Evm, Txs, Attrs> OpPayloadBuilder<Pool, Client, Evm, Txs, Attrs> {
    /// Sets the rollup's compute pending block configuration option.
    pub const fn set_compute_pending_block(mut self, compute_pending_block: bool) -> Self {
        self.compute_pending_block = compute_pending_block;
        self
    }

    /// Configures the type responsible for yielding the transactions that should be included in the
    /// payload.
    pub fn with_transactions<T>(
        self,
        best_transactions: T,
    ) -> OpPayloadBuilder<Pool, Client, Evm, T, Attrs> {
        let Self { pool, client, compute_pending_block, evm_config, config, .. } = self;
        OpPayloadBuilder {
            pool,
            client,
            compute_pending_block,
            evm_config,
            best_transactions,
            config,
            _pd: PhantomData,
        }
    }

    /// Enables the rollup's compute pending block configuration option.
    pub const fn compute_pending_block(self) -> Self {
        self.set_compute_pending_block(true)
    }

    /// Returns the rollup's compute pending block configuration option.
    pub const fn is_compute_pending_block(&self) -> bool {
        self.compute_pending_block
    }

    /// Computes the witness for the payload.
    pub fn payload_witness(
        &self,
        _parent: SealedHeader<<Evm::Primitives as NodePrimitives>::BlockHeader>,
        _attributes: Attrs::RpcPayloadAttributes,
    ) -> Result<ExecutionWitness, PayloadBuilderError>
    where
        Evm: ConfigureEvm,
        Evm::Primitives: OpPayloadPrimitives,
        Attrs: OpAttributes<Transaction = TxTy<Evm::Primitives>>,
    {
        Err(PayloadBuilderError::other(
            OpPayloadBuilderError::PayloadBuilderUnavailable,
        ))
    }
}

impl<Pool, Client, Evm, N, Txs> PayloadBuilder
    for OpPayloadBuilder<Pool, Client, Evm, Txs, OpPayloadBuilderAttributes<N::SignedTx>>
where
    Pool: Send + Sync + Unpin + Clone + 'static,
    Client: Send + Sync + Clone + 'static,
    Evm: ConfigureEvm<Primitives = N> + 'static,
    N: OpPayloadPrimitives,
    Txs: Send + Sync + Unpin + Clone + 'static,
{
    type Attributes = OpPayloadAttrs;
    type BuiltPayload = OpBuiltPayload<N>;

    fn try_build(
        &self,
        _args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError> {
        Err(PayloadBuilderError::other(
            OpPayloadBuilderError::PayloadBuilderUnavailable,
        ))
    }

    fn on_missing_payload(
        &self,
        _args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> MissingPayloadBehaviour<Self::BuiltPayload> {
        MissingPayloadBehaviour::RaceEmptyPayload
    }

    fn build_empty_payload(
        &self,
        _config: PayloadConfig<Self::Attributes, <N as NodePrimitives>::BlockHeader>,
    ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        Err(PayloadBuilderError::other(
            OpPayloadBuilderError::PayloadBuilderUnavailable,
        ))
    }
}

/// Compatibility shell for the upstream witness-only builder helper.
#[derive(Debug)]
pub struct OpBuilder<'a, Txs> {
    _pd: PhantomData<&'a Txs>,
}

impl<'a, Txs> OpBuilder<'a, Txs> {
    /// Creates a new [`OpBuilder`].
    pub fn new(_best: impl FnOnce(BestTransactionsAttributes) -> Txs + Send + Sync + 'a) -> Self {
        Self { _pd: PhantomData }
    }
}

impl<Txs> OpBuilder<'_, Txs> {
    /// Builds the payload and returns its [`ExecutionWitness`] based on the state after execution.
    pub fn witness<Evm, ChainSpec, N, Attrs, StateProvider>(
        self,
        _state_provider: StateProvider,
        _ctx: &OpPayloadBuilderCtx<Evm, ChainSpec, Attrs>,
    ) -> Result<ExecutionWitness, PayloadBuilderError>
    where
        Evm: ConfigureEvm<Primitives = N>,
        ChainSpec: EthChainSpec + OpHardforks,
        N: OpPayloadPrimitives,
        Attrs: OpAttributes<Transaction = N::SignedTx>,
    {
        Err(PayloadBuilderError::other(
            OpPayloadBuilderError::PayloadBuilderUnavailable,
        ))
    }
}

/// A type that returns a the [`PayloadTransactions`] that should be included in the pool.
pub trait OpPayloadTransactions<Transaction>: Clone + Send + Sync + Unpin + 'static {
    /// Returns an iterator that yields the transaction in the order they should get included in the
    /// new payload.
    fn best_transactions<Pool: TransactionPool<Transaction = Transaction>>(
        &self,
        pool: Pool,
        attr: BestTransactionsAttributes,
    ) -> impl PayloadTransactions<Transaction = Transaction>;
}

impl<T: PoolTransaction + MaybeInteropTransaction> OpPayloadTransactions<T> for () {
    fn best_transactions<Pool: TransactionPool<Transaction = T>>(
        &self,
        pool: Pool,
        attr: BestTransactionsAttributes,
    ) -> impl PayloadTransactions<Transaction = T> {
        BestPayloadTransactions::new(pool.best_transactions_with_attributes(attr))
    }
}

/// Holds the state after execution.
#[derive(Debug)]
pub struct ExecutedPayload<N: NodePrimitives> {
    /// Tracked execution info.
    pub info: ExecutionInfo,
    /// Withdrawal hash.
    pub withdrawals_root: Option<B256>,
    /// The transaction receipts.
    pub receipts: Vec<N::Receipt>,
    /// The block env used during execution.
    pub block_env: BlockEnv,
}

/// This acts as the container for executed transactions and its byproducts (receipts, gas used).
#[derive(Default, Debug)]
pub struct ExecutionInfo {
    /// All gas used so far.
    pub cumulative_gas_used: u64,
    /// Estimated DA size.
    pub cumulative_da_bytes_used: u64,
    /// Tracks fees from executed mempool transactions.
    pub total_fees: U256,
}

impl ExecutionInfo {
    /// Create a new instance with allocated slots.
    pub const fn new() -> Self {
        Self { cumulative_gas_used: 0, cumulative_da_bytes_used: 0, total_fees: U256::ZERO }
    }

    /// Returns true if the transaction would exceed the block limits.
    pub fn is_tx_over_limits(
        &self,
        tx_da_size: u64,
        block_gas_limit: u64,
        tx_data_limit: Option<u64>,
        block_data_limit: Option<u64>,
        tx_gas_limit: u64,
        da_footprint_gas_scalar: Option<u16>,
    ) -> bool {
        if tx_data_limit.is_some_and(|da_limit| tx_da_size > da_limit) {
            return true;
        }

        let total_da_bytes_used = self.cumulative_da_bytes_used.saturating_add(tx_da_size);

        if block_data_limit.is_some_and(|da_limit| total_da_bytes_used > da_limit) {
            return true;
        }

        if let Some(da_footprint_gas_scalar) = da_footprint_gas_scalar {
            let tx_da_footprint =
                total_da_bytes_used.saturating_mul(da_footprint_gas_scalar as u64);
            if tx_da_footprint > block_gas_limit {
                return true;
            }
        }

        self.cumulative_gas_used + tx_gas_limit > block_gas_limit
    }
}

/// Container type that holds all necessities to build a new payload.
#[derive(derive_more::Debug)]
pub struct OpPayloadBuilderCtx<
    Evm: ConfigureEvm,
    ChainSpec,
    Attrs = OpPayloadBuilderAttributes<TxTy<<Evm as ConfigureEvm>::Primitives>>,
> {
    /// The type that knows how to perform system calls and configure the evm.
    pub evm_config: Evm,
    /// Additional config for the builder/sequencer, e.g. DA and gas limit.
    pub builder_config: OpBuilderConfig,
    /// The chainspec.
    pub chain_spec: Arc<ChainSpec>,
    /// How to build the payload.
    pub config: PayloadConfig<Attrs, HeaderTy<Evm::Primitives>>,
    /// Marker to check whether the job has been cancelled.
    pub cancel: CancelOnDrop,
    /// The currently best payload.
    pub best_payload: Option<OpBuiltPayload<Evm::Primitives>>,
}

impl<Evm, ChainSpec, Attrs> OpPayloadBuilderCtx<Evm, ChainSpec, Attrs>
where
    Evm: ConfigureEvm<
            Primitives: OpPayloadPrimitives,
            NextBlockEnvCtx: BuildNextEnv<Attrs, HeaderTy<Evm::Primitives>, ChainSpec>,
        >,
    ChainSpec: EthChainSpec + OpHardforks,
    Attrs: OpAttributes<Transaction = TxTy<Evm::Primitives>>,
{
    /// Returns the parent block the payload will be build on.
    pub fn parent(&self) -> &SealedHeaderFor<Evm::Primitives> {
        self.config.parent_header.as_ref()
    }

    /// Returns the builder attributes.
    pub const fn attributes(&self) -> &Attrs {
        &self.config.attributes
    }

    /// Returns the current fee settings for transactions from the mempool.
    pub fn best_transaction_attributes(&self, block_env: impl Block) -> BestTransactionsAttributes {
        BestTransactionsAttributes::new(
            block_env.basefee(),
            block_env.blob_gasprice().map(|p| p as u64),
        )
    }

    /// Returns the unique id for this payload job.
    pub fn payload_id(&self) -> PayloadId {
        self.attributes().payload_id()
    }

    /// Returns true if the fees are higher than the previous payload.
    pub fn is_better_payload(&self, total_fees: U256) -> bool {
        is_better_payload(self.best_payload.as_ref(), total_fees)
    }

    /// Prepares a [`BlockBuilder`] for the next block.
    pub fn block_builder<'a, DB: Database>(
        &'a self,
        db: &'a mut State<DB>,
    ) -> Result<
        impl BlockBuilder<
            Primitives = Evm::Primitives,
            Executor: BlockExecutorFor<'a, Evm::BlockExecutorFactory, &'a mut State<DB>>,
        > + 'a,
        PayloadBuilderError,
    > {
        self.evm_config
            .builder_for_next_block(
                db,
                self.parent(),
                Evm::NextBlockEnvCtx::build_next_env(
                    self.attributes(),
                    self.parent(),
                    self.chain_spec.as_ref(),
                )
                .map_err(PayloadBuilderError::other)?,
            )
            .map_err(PayloadBuilderError::other)
    }

    /// Executes all sequencer transactions that are included in the payload attributes.
    pub fn execute_sequencer_transactions(
        &self,
        builder: &mut impl BlockBuilder<Primitives = Evm::Primitives>,
    ) -> Result<ExecutionInfo, PayloadBuilderError> {
        let mut info = ExecutionInfo::new();

        for sequencer_tx in self.attributes().sequencer_transactions() {
            if sequencer_tx.value().is_eip4844() {
                return Err(PayloadBuilderError::other(
                    OpPayloadBuilderError::BlobTransactionRejected,
                ));
            }

            let sequencer_tx = sequencer_tx.value().try_clone_into_recovered().map_err(|_| {
                PayloadBuilderError::other(OpPayloadBuilderError::TransactionEcRecoverFailed)
            })?;

            let gas_used = match builder.execute_transaction(sequencer_tx.clone()) {
                Ok(gas_used) => gas_used,
                Err(BlockExecutionError::Validation(BlockValidationError::InvalidTx {
                    error,
                    ..
                })) => {
                    trace!(target: "payload_builder", %error, ?sequencer_tx, "Error in sequencer transaction, skipping.");
                    continue;
                }
                Err(err) => return Err(PayloadBuilderError::EvmExecutionError(Box::new(err))),
            };

            info.cumulative_gas_used += gas_used;
        }

        Ok(info)
    }

    /// Executes the given best transactions and updates the execution info.
    pub fn execute_best_transactions<Builder>(
        &self,
        info: &mut ExecutionInfo,
        builder: &mut Builder,
        mut best_txs: impl PayloadTransactions<
            Transaction: PoolTransaction<Consensus = TxTy<Evm::Primitives>> + OpPooledTx,
        >,
    ) -> Result<Option<()>, PayloadBuilderError>
    where
        Builder: BlockBuilder<Primitives = Evm::Primitives>,
        <<Builder::Executor as BlockExecutor>::Evm as AlloyEvm>::DB: Database,
    {
        let mut block_gas_limit = builder.evm_mut().block().gas_limit();
        if let Some(gas_limit_config) = self.builder_config.gas_limit_config.gas_limit() {
            block_gas_limit = gas_limit_config.min(block_gas_limit);
        };
        let block_da_limit = self.builder_config.da_config.max_da_block_size();
        let tx_da_limit = self.builder_config.da_config.max_da_tx_size();
        let base_fee = builder.evm_mut().block().basefee();

        while let Some(tx) = best_txs.next(()) {
            let interop = tx.interop_deadline();
            let tx_da_size = tx.estimated_da_size();
            let tx = tx.into_consensus();

            let da_footprint_gas_scalar = self
                .chain_spec
                .is_jovian_active_at_timestamp(self.attributes().timestamp())
                .then_some(
                    L1BlockInfo::fetch_da_footprint_gas_scalar(builder.evm_mut().db_mut()).expect(
                        "DA footprint should always be available from the database post jovian",
                    ),
                );

            if info.is_tx_over_limits(
                tx_da_size,
                block_gas_limit,
                tx_da_limit,
                block_da_limit,
                tx.gas_limit(),
                da_footprint_gas_scalar,
            ) {
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            if tx.is_eip4844() || tx.is_deposit() {
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            if let Some(interop) = interop &&
                !is_valid_interop(interop, self.config.attributes.timestamp())
            {
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            if self.cancel.is_cancelled() {
                return Ok(Some(()));
            }

            let gas_used = match builder.execute_transaction(tx.clone()) {
                Ok(gas_used) => gas_used,
                Err(BlockExecutionError::Validation(BlockValidationError::InvalidTx {
                    error,
                    ..
                })) => {
                    if error.is_nonce_too_low() {
                        trace!(target: "payload_builder", %error, ?tx, "skipping nonce too low transaction");
                    } else {
                        trace!(target: "payload_builder", %error, ?tx, "skipping invalid transaction and its descendants");
                        best_txs.mark_invalid(tx.signer(), tx.nonce());
                    }
                    continue;
                }
                Err(err) => return Err(PayloadBuilderError::EvmExecutionError(Box::new(err))),
            };

            info.cumulative_gas_used += gas_used;
            info.cumulative_da_bytes_used += tx_da_size;

            let miner_fee = tx
                .effective_tip_per_gas(base_fee)
                .expect("fee is always valid; execution succeeded");
            info.total_fees += U256::from(miner_fee) * U256::from(gas_used);
        }

        Ok(None)
    }
}
