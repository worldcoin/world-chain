use std::fmt::Display;
use std::sync::Arc;

use alloy_consensus::constants::EMPTY_WITHDRAWALS;
use alloy_consensus::{proofs, Eip658Value, Transaction, EMPTY_OMMER_ROOT_HASH};
use alloy_eips::eip4895::Withdrawals;
use alloy_eips::merge::BEACON_NONCE;
use alloy_eips::Typed2718;
use alloy_rlp::Encodable;
use alloy_rpc_types_debug::ExecutionWitness;
use op_alloy_consensus::{OpDepositReceipt, OpTxType};
use reth::api::PayloadBuilderError;
use reth::payload::{PayloadBuilderAttributes, PayloadId};
use reth::revm::database::StateProviderDatabase;
use reth::revm::db::states::bundle_state::BundleRetention;
use reth::revm::witness::ExecutionWitnessRecord;
use reth::revm::{DatabaseCommit, State};
use reth::transaction_pool::{BestTransactionsAttributes, TransactionPool};
use reth_basic_payload_builder::{
    is_better_payload, BuildArguments, BuildOutcome, BuildOutcomeKind, MissingPayloadBehaviour,
    PayloadBuilder, PayloadConfig,
};
use reth_chain_state::{ExecutedBlock, ExecutedBlockWithTrieUpdates};
use reth_evm::env::EvmEnv;
use reth_evm::{ConfigureEvm, ConfigureEvmEnv, ConfigureEvmFor, Evm};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_consensus::calculate_receipt_root_no_memo_optimism;
use reth_optimism_node::{
    OpBuiltPayload, OpPayloadBuilder, OpPayloadBuilderAttributes, OpReceiptBuilder,
    ReceiptBuilderCtx,
};
use reth_optimism_payload_builder::builder::{
    ExecutedPayload, ExecutionInfo, OpPayloadBuilderCtx, OpPayloadTransactions,
};
use reth_optimism_payload_builder::config::OpBuilderConfig;
use reth_optimism_payload_builder::{OpPayloadAttributes, OpPayloadPrimitives};
use reth_optimism_primitives::transaction::signed::OpTransaction;
use reth_optimism_primitives::{
    OpPrimitives, OpReceipt, OpTransactionSigned, ADDRESS_L2_TO_L1_MESSAGE_PASSER,
};
use reth_payload_util::{NoopPayloadTransactions, PayloadTransactions};
use reth_primitives::{
    Block, BlockBody, Header, InvalidTransactionError, NodePrimitives, RecoveredBlock, SealedHeader,
};
use reth_primitives_traits::Block as _;
use reth_provider::{
    BlockReaderIdExt, ChainSpecProvider, ExecutionOutcome, HashedPostStateProvider, ProviderError,
    StateProofProvider, StateProviderFactory, StateRootProvider, StorageRootProvider,
};
use reth_transaction_pool::error::InvalidPoolTransactionError;
use reth_transaction_pool::{BestTransactions, PoolTransaction, ValidPoolTransaction};
use revm::Database;
use revm_primitives::{
    Address, Bytes, EVMError, ExecutionResult, InvalidTransaction, ResultAndState, B256, U256,
};
use tracing::{debug, trace, warn};
use world_chain_builder_pool::noop::NoopWorldChainTransactionPool;
use world_chain_builder_pool::tx::{WorldChainPoolTransaction, WorldChainPoolTransactionError};
use world_chain_builder_rpc::transactions::validate_conditional_options;

use crate::inspector::{PBHCallTracer, PBH_CALL_TRACER_ERROR};

/// World Chain payload builder
#[derive(Debug, Clone)]
pub struct WorldChainPayloadBuilder<Pool, Client, EvmConfig, N: NodePrimitives, Txs = ()> {
    pub inner: OpPayloadBuilder<Pool, Client, EvmConfig, N, Txs>,
    pub verified_blockspace_capacity: u8,
    pub pbh_entry_point: Address,
    pub pbh_signature_aggregator: Address,
}

impl<Pool, Client, EvmConfig, N: NodePrimitives>
    WorldChainPayloadBuilder<Pool, Client, EvmConfig, N>
where
    EvmConfig: ConfigureEvm<Header = Header>,
{
    pub fn new(
        pool: Pool,
        client: Client,
        evm_config: EvmConfig,
        receipt_builder: impl OpReceiptBuilder<N::SignedTx, Receipt = N::Receipt>,
        compute_pending_block: bool,
        verified_blockspace_capacity: u8,
        pbh_entry_point: Address,
        pbh_signature_aggregator: Address,
    ) -> Self {
        Self::with_builder_config(
            pool,
            client,
            evm_config,
            receipt_builder,
            compute_pending_block,
            OpBuilderConfig::default(),
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
        )
    }

    pub fn with_builder_config(
        pool: Pool,
        client: Client,
        evm_config: EvmConfig,
        receipt_builder: impl OpReceiptBuilder<N::SignedTx, Receipt = N::Receipt>,
        compute_pending_block: bool,
        config: OpBuilderConfig,
        verified_blockspace_capacity: u8,
        pbh_entry_point: Address,
        pbh_signature_aggregator: Address,
    ) -> Self {
        let inner = OpPayloadBuilder::with_builder_config(
            pool,
            client,
            evm_config,
            receipt_builder,
            config,
        )
        .set_compute_pending_block(compute_pending_block);

        Self {
            inner,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
        }
    }
}

impl<Pool, Client, EvmConfig, N: NodePrimitives>
    WorldChainPayloadBuilder<Pool, Client, EvmConfig, N>
{
    /// Sets the rollup's compute pending block configuration option.
    pub const fn set_compute_pending_block(mut self, compute_pending_block: bool) -> Self {
        self.inner.compute_pending_block = compute_pending_block;
        self
    }

    pub fn with_transactions<T>(
        self,
        best_transactions: T,
    ) -> WorldChainPayloadBuilder<Pool, Client, EvmConfig, N, T> {
        let Self {
            inner,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
        } = self;

        let OpPayloadBuilder {
            compute_pending_block,
            evm_config,
            config,
            pool,
            client,
            receipt_builder,
            ..
        } = inner;

        WorldChainPayloadBuilder {
            inner: OpPayloadBuilder {
                compute_pending_block,
                evm_config,
                config,
                pool,
                client,
                receipt_builder,
                best_transactions,
            },
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
        }
    }

    /// Enables the rollup's compute pending block configuration option.
    pub const fn compute_pending_block(self) -> Self {
        self.set_compute_pending_block(true)
    }

    /// Returns the rollup's compute pending block configuration option.
    pub const fn is_compute_pending_block(&self) -> bool {
        self.inner.compute_pending_block
    }
}

impl<Pool, Client, EvmConfig, N, T> WorldChainPayloadBuilder<Pool, Client, EvmConfig, N, T>
where
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = N::SignedTx>>,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec = OpChainSpec>,
    N: OpPayloadPrimitives,
    EvmConfig: ConfigureEvmFor<N>,
{
    /// Constructs an Optimism payload from the transactions sent via the
    /// Payload attributes by the sequencer. If the `no_tx_pool` argument is passed in
    /// the payload attributes, the transaction pool will be ignored and the only transactions
    /// included in the payload will be those sent through the attributes.
    ///
    /// Given build arguments including an Optimism client, transaction pool,
    /// and configuration, this function creates a transaction payload. Returns
    /// a result indicating success with the payload or an error in case of failure.
    fn build_payload<'a, Txs>(
        &self,
        args: BuildArguments<OpPayloadBuilderAttributes<N::SignedTx>, OpBuiltPayload<N>>,
        best: impl FnOnce(BestTransactionsAttributes) -> Txs + Send + Sync + 'a,
    ) -> Result<BuildOutcome<OpBuiltPayload<N>>, PayloadBuilderError>
    where
        Txs: PayloadTransactions<Transaction: PoolTransaction<Consensus = N::SignedTx>>,
    {
        let evm_env = self
            .evm_env(&args.config.attributes, &args.config.parent_header)
            .map_err(PayloadBuilderError::other)?;

        let BuildArguments {
            mut cached_reads,
            config,
            cancel,
            best_payload,
        } = args;

        let ctx = WorldChainPayloadBuilderCtx {
            inner: OpPayloadBuilderCtx {
                evm_config: self.inner.evm_config.clone(),
                da_config: self.inner.config.da_config.clone(),
                chain_spec: self.inner.client.chain_spec(),
                config,
                evm_env,
                cancel,
                best_payload,
                receipt_builder: self.inner.receipt_builder.clone(),
            },
            verified_blockspace_capacity: self.verified_blockspace_capacity,
            pbh_entry_point: self.pbh_entry_point,
            pbh_signature_aggregator: self.pbh_signature_aggregator,
        };

        let op_ctx = &ctx.inner;
        let builder = WorldChainBuilder::new(best);
        let state_provider = self
            .inner
            .client
            .state_by_block_hash(op_ctx.parent().hash())?;
        let state = StateProviderDatabase::new(state_provider);

        if op_ctx.attributes().no_tx_pool {
            let db = State::builder()
                .with_database(state)
                .with_bundle_update()
                .build();

            builder.build(db, ctx, &self.inner.pool)
        } else {
            // sequencer mode we can reuse cachedreads from previous runs
            let db = State::builder()
                .with_database(cached_reads.as_db_mut(state))
                .with_bundle_update()
                .build();
            builder.build(db, ctx, &self.inner.pool)
        }
        .map(|out| out.with_cached_reads(cached_reads))
    }

    /// Returns the configured [`EvmEnv`] for the targeted payload
    /// (that has the `parent` as its parent).
    pub fn evm_env(
        &self,
        attributes: &OpPayloadBuilderAttributes<N::SignedTx>,
        parent: &Header,
    ) -> Result<EvmEnv<EvmConfig::Spec>, EvmConfig::Error> {
        self.inner.evm_env(attributes, parent)
    }

    /// Computes the witness for the payload.
    pub fn payload_witness(
        &self,
        parent: SealedHeader,
        attributes: OpPayloadAttributes,
    ) -> Result<ExecutionWitness, PayloadBuilderError> {
        let attributes = OpPayloadBuilderAttributes::try_new(parent.hash(), attributes, 3)
            .map_err(PayloadBuilderError::other)?;

        let evm_env = self
            .evm_env(&attributes, &parent)
            .map_err(PayloadBuilderError::other)?;

        let config = PayloadConfig {
            parent_header: Arc::new(parent),
            attributes,
        };
        let ctx = WorldChainPayloadBuilderCtx {
            inner: OpPayloadBuilderCtx {
                evm_config: self.inner.evm_config.clone(),
                da_config: self.inner.config.da_config.clone(),
                chain_spec: self.inner.client.chain_spec(),
                config,
                evm_env,
                cancel: Default::default(),
                best_payload: Default::default(),
                receipt_builder: self.inner.receipt_builder.clone(),
            },
            verified_blockspace_capacity: self.verified_blockspace_capacity,
            pbh_entry_point: self.pbh_entry_point,
            pbh_signature_aggregator: self.pbh_signature_aggregator,
        };

        let state_provider = self
            .inner
            .client
            .state_by_block_hash(ctx.inner.parent().hash())?;
        let state = StateProviderDatabase::new(state_provider);
        let mut state = State::builder()
            .with_database(state)
            .with_bundle_update()
            .build();

        let builder =
            WorldChainBuilder::new(|_| NoopPayloadTransactions::<Pool::Transaction>::default());

        builder.witness(&mut state, &ctx, &self.inner.pool)
    }
}

/// Implementation of the [`PayloadBuilder`] trait for [`WorldChainPayloadBuilder`].
impl<Pool, Client, EvmConfig, N, Txs> PayloadBuilder
    for WorldChainPayloadBuilder<Pool, Client, EvmConfig, N, Txs>
where
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec = OpChainSpec> + Clone,
    N: OpPayloadPrimitives,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = N::SignedTx>>,
    EvmConfig: ConfigureEvmFor<N>,
    Txs: OpPayloadTransactions<Pool::Transaction>,
{
    type Attributes = OpPayloadBuilderAttributes<N::SignedTx>;
    type BuiltPayload = OpBuiltPayload<N>;

    fn try_build(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError> {
        let pool = self.inner.pool.clone();
        self.build_payload(args, |attrs| {
            self.inner.best_transactions.best_transactions(pool, attrs)
        })
    }

    fn on_missing_payload(
        &self,
        _args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> MissingPayloadBehaviour<Self::BuiltPayload> {
        // we want to await the job that's already in progress because that should be returned as
        // is, there's no benefit in racing another job
        MissingPayloadBehaviour::AwaitInProgress
    }

    // NOTE: this should only be used for testing purposes because this doesn't have access to L1
    // system txs, hence on_missing_payload we return [MissingPayloadBehaviour::AwaitInProgress].
    fn build_empty_payload(
        &self,
        config: PayloadConfig<Self::Attributes>,
    ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        let args = BuildArguments {
            config,
            cached_reads: Default::default(),
            cancel: Default::default(),
            best_payload: None,
        };
        self.build_payload(args, |_| {
            NoopPayloadTransactions::<Pool::Transaction>::default()
        })?
        .into_payload()
        .ok_or_else(|| PayloadBuilderError::MissingPayload)
    }
}

/// The type that builds the payload.
///
/// Payload building for optimism is composed of several steps.
/// The first steps are mandatory and defined by the protocol.
///
/// 1. first all System calls are applied.
/// 2. After canyon the forced deployed `create2deployer` must be loaded
/// 3. all sequencer transactions are executed (part of the payload attributes)
///
/// Depending on whether the node acts as a sequencer and is allowed to include additional
/// transactions (`no_tx_pool == false`):
/// 4. include additional transactions
///
/// And finally
/// 5. build the block: compute all roots (txs, state)
#[derive(derive_more::Debug)]
pub struct WorldChainBuilder<'a, Txs> {
    /// Yields the best transaction to include if transactions from the mempool are allowed.
    #[debug(skip)]
    best: Box<dyn FnOnce(BestTransactionsAttributes) -> Txs + 'a>,
}

impl<'a, Txs> WorldChainBuilder<'a, Txs> {
    fn new(best: impl FnOnce(BestTransactionsAttributes) -> Txs + Send + Sync + 'a) -> Self {
        Self {
            best: Box::new(best),
        }
    }
}

impl<Txs> WorldChainBuilder<'_, Txs> {
    /// Executes the payload and returns the outcome.
    pub fn execute<EvmConfig, N, DB, P, Pool>(
        self,
        state: &mut State<DB>,
        ctx: &WorldChainPayloadBuilderCtx<EvmConfig, N>,
        pool: &Pool,
    ) -> Result<BuildOutcomeKind<ExecutedPayload<N>>, PayloadBuilderError>
    where
        N: OpPayloadPrimitives,
        Txs: PayloadTransactions<Transaction: PoolTransaction<Consensus = N::SignedTx>>,
        EvmConfig: ConfigureEvmFor<N>,
        DB: Database<Error = ProviderError> + AsRef<P>,
        P: StorageRootProvider,
        Pool: TransactionPool<Transaction: PoolTransaction<Consensus = N::SignedTx>>,
    {
        let Self { best } = self;

        let op_ctx = &ctx.inner;
        debug!(target: "payload_builder", id=%op_ctx.payload_id(), parent_header = ?ctx.inner.parent().hash(), parent_number = ctx.inner.parent().number, "building new payload");

        // 1. apply eip-4788 pre block contract call
        op_ctx.apply_pre_beacon_root_contract_call(state)?;

        // 2. ensure create2deployer is force deployed
        op_ctx.ensure_create2_deployer(state)?;

        // 3. execute sequencer transactions
        let mut info = op_ctx.execute_sequencer_transactions(state)?;

        // 4. if mem pool transactions are requested we execute them
        if !op_ctx.attributes().no_tx_pool {
            let best_txs = best(ctx.inner.best_transaction_attributes());
            if ctx
                .execute_best_transactions(&mut info, state, best_txs, pool)?
                .is_some()
            {
                return Ok(BuildOutcomeKind::Cancelled);
            }

            // check if the new payload is even more valuable
            if !ctx.inner.is_better_payload(info.total_fees) {
                // can skip building the block
                return Ok(BuildOutcomeKind::Aborted {
                    fees: info.total_fees,
                });
            }
        }

        // merge all transitions into bundle state, this would apply the withdrawal balance changes
        // and 4788 contract call
        state.merge_transitions(BundleRetention::Reverts);

        let withdrawals_root = if op_ctx.is_isthmus_active() {
            // withdrawals root field in block header is used for storage root of L2 predeploy
            // `l2tol1-message-passer`
            Some(
                state
                    .database
                    .as_ref()
                    .storage_root(ADDRESS_L2_TO_L1_MESSAGE_PASSER, Default::default())?,
            )
        } else if op_ctx.is_canyon_active() {
            Some(EMPTY_WITHDRAWALS)
        } else {
            None
        };

        let payload = ExecutedPayload {
            info,
            withdrawals_root,
        };

        Ok(BuildOutcomeKind::Better { payload })
    }

    /// Builds the payload on top of the state.
    pub fn build<EvmConfig, N, DB, P, Pool>(
        self,
        mut state: State<DB>,
        ctx: WorldChainPayloadBuilderCtx<EvmConfig, N>,
        pool: &Pool,
    ) -> Result<BuildOutcomeKind<OpBuiltPayload<N>>, PayloadBuilderError>
    where
        EvmConfig: ConfigureEvmFor<N>,
        N: OpPayloadPrimitives,
        Txs: PayloadTransactions<Transaction: PoolTransaction<Consensus = N::SignedTx>>,
        DB: Database<Error = ProviderError> + AsRef<P>,
        P: StateRootProvider + HashedPostStateProvider + StorageRootProvider,
        Pool: TransactionPool<Transaction: PoolTransaction<Consensus = N::SignedTx>>,
    {
        let ExecutedPayload {
            info,
            withdrawals_root,
        } = match self.execute(&mut state, &ctx, pool)? {
            BuildOutcomeKind::Better { payload } | BuildOutcomeKind::Freeze(payload) => payload,
            BuildOutcomeKind::Cancelled => return Ok(BuildOutcomeKind::Cancelled),
            BuildOutcomeKind::Aborted { fees } => return Ok(BuildOutcomeKind::Aborted { fees }),
        };

        let op_ctx = ctx.inner;
        let block_number = op_ctx.block_number();
        let execution_outcome = ExecutionOutcome::new(
            state.take_bundle(),
            vec![info.receipts],
            block_number,
            Vec::new(),
        );
        let receipts_root = execution_outcome
            .generic_receipts_root_slow(block_number, |receipts| {
                calculate_receipt_root_no_memo_optimism(
                    receipts,
                    &op_ctx.chain_spec,
                    op_ctx.attributes().timestamp(),
                )
            })
            .expect("Number is in range");
        let logs_bloom = execution_outcome
            .block_logs_bloom(block_number)
            .expect("Number is in range");

        // // calculate the state root
        let state_provider = state.database.as_ref();
        let hashed_state = state_provider.hashed_post_state(execution_outcome.state());
        let (state_root, trie_output) = {
            state_provider
                .state_root_with_updates(hashed_state.clone())
                .inspect_err(|err| {
                    warn!(target: "payload_builder",
                    parent_header=%op_ctx.parent().hash(),
                        %err,
                        "failed to calculate state root for payload"
                    );
                })?
        };

        // create the block header
        let transactions_root = proofs::calculate_transaction_root(&info.executed_transactions);

        // OP doesn't support blobs/EIP-4844.
        // https://specs.optimism.io/protocol/exec-engine.html#ecotone-disable-blob-transactions
        // Need [Some] or [None] based on hardfork to match block hash.
        let (excess_blob_gas, blob_gas_used) = op_ctx.blob_fields();
        let extra_data = op_ctx.extra_data()?;

        let header = Header {
            parent_hash: op_ctx.parent().hash(),
            ommers_hash: EMPTY_OMMER_ROOT_HASH,
            beneficiary: op_ctx.evm_env.block_env.coinbase,
            state_root,
            transactions_root,
            receipts_root,
            withdrawals_root,
            logs_bloom,
            timestamp: op_ctx.attributes().payload_attributes.timestamp,
            mix_hash: op_ctx.attributes().payload_attributes.prev_randao,
            nonce: BEACON_NONCE.into(),
            base_fee_per_gas: Some(op_ctx.base_fee()),
            number: op_ctx.parent().number + 1,
            gas_limit: op_ctx.block_gas_limit(),
            difficulty: U256::ZERO,
            gas_used: info.cumulative_gas_used,
            extra_data,
            parent_beacon_block_root: op_ctx
                .attributes()
                .payload_attributes
                .parent_beacon_block_root,
            blob_gas_used,
            excess_blob_gas,
            requests_hash: None,
        };

        // seal the block
        let block = N::Block::new(
            header,
            BlockBody {
                transactions: info.executed_transactions,
                ommers: vec![],
                withdrawals: op_ctx.withdrawals().cloned(),
            },
        );

        let sealed_block = Arc::new(block.seal_slow());
        debug!(target: "payload_builder", id=%op_ctx.attributes().payload_id(), sealed_block_header = ?sealed_block.header(), "sealed built block");

        // create the executed block data
        let executed: ExecutedBlockWithTrieUpdates<N> = ExecutedBlockWithTrieUpdates {
            block: ExecutedBlock {
                recovered_block: Arc::new(RecoveredBlock::new_sealed(
                    sealed_block.as_ref().clone(),
                    info.executed_senders,
                )),
                execution_output: Arc::new(execution_outcome),
                hashed_state: Arc::new(hashed_state),
            },
            trie: Arc::new(trie_output),
        };

        let no_tx_pool = op_ctx.attributes().no_tx_pool;

        let payload = OpBuiltPayload::new(
            op_ctx.payload_id(),
            sealed_block,
            info.total_fees,
            Some(executed),
        );

        if no_tx_pool {
            // if `no_tx_pool` is set only transactions from the payload attributes will be included
            // in the payload. In other words, the payload is deterministic and we can
            // freeze it once we've successfully built it.
            Ok(BuildOutcomeKind::Freeze(payload))
        } else {
            Ok(BuildOutcomeKind::Better { payload })
        }
    }

    /// Builds the payload and returns its [`ExecutionWitness`] based on the state after execution.
    pub fn witness<EvmConfig, N, DB, P, Pool>(
        self,
        state: &mut State<DB>,
        ctx: &WorldChainPayloadBuilderCtx<EvmConfig, N>,
        pool: &Pool,
    ) -> Result<ExecutionWitness, PayloadBuilderError>
    where
        EvmConfig: ConfigureEvmFor<N>,
        N: OpPayloadPrimitives,
        Txs: PayloadTransactions<Transaction: PoolTransaction<Consensus = N::SignedTx>>,
        DB: Database<Error = ProviderError> + AsRef<P>,
        P: StateProofProvider + StorageRootProvider,
        Pool: TransactionPool<Transaction: PoolTransaction<Consensus = N::SignedTx>>,
    {
        let _ = self.execute(state, ctx, pool)?;
        let ExecutionWitnessRecord {
            hashed_state,
            codes,
            keys,
        } = ExecutionWitnessRecord::from_executed_state(state);
        let state = state
            .database
            .as_ref()
            .witness(Default::default(), hashed_state)?;
        Ok(ExecutionWitness {
            state: state.into_iter().collect(),
            codes,
            keys,
        })
    }
}

/// Container type that holds all necessities to build a new payload.
#[derive(Debug)]
pub struct WorldChainPayloadBuilderCtx<EvmConfig: ConfigureEvmEnv, N: NodePrimitives> {
    pub inner: OpPayloadBuilderCtx<EvmConfig, N>,
    pub verified_blockspace_capacity: u8,
    pub pbh_entry_point: Address,
    pub pbh_signature_aggregator: Address,
}

impl<EvmConfig, N> WorldChainPayloadBuilderCtx<EvmConfig, N>
where
    EvmConfig: ConfigureEvmFor<N>,
    N: OpPayloadPrimitives,
{
    /// Executes the given best transactions and updates the execution info.
    ///
    /// Returns `Ok(Some(())` if the job was cancelled.
    pub fn execute_best_transactions<DB, Pool>(
        &self,
        info: &mut ExecutionInfo<N>,
        db: &mut State<DB>,
        mut best_txs: impl PayloadTransactions<
            Transaction: PoolTransaction<Consensus = EvmConfig::Transaction>,
        >,
        pool: &Pool,
    ) -> Result<Option<()>, PayloadBuilderError>
    where
        DB: Database<Error = ProviderError>,
        Pool: TransactionPool<Transaction: PoolTransaction<Consensus = N::SignedTx>>,
    {
        let block_gas_limit = self.inner.block_gas_limit();
        let block_da_limit = self.inner.da_config.max_da_block_size();
        let tx_da_limit = self.inner.da_config.max_da_tx_size();
        let base_fee = self.inner.base_fee();

        let mut pbh_call_tracer =
            PBHCallTracer::new(self.pbh_entry_point, self.pbh_signature_aggregator);

        let mut evm = self.inner.evm_config.evm_with_env_and_inspector(
            &mut *db,
            self.inner.evm_env.clone(),
            &mut pbh_call_tracer,
        );

        let mut invalid_txs = vec![];
        let verified_gas_limit = (self.verified_blockspace_capacity as u64 * block_gas_limit) / 100;
        while let Some(pooled_tx) = best_txs.next(()) {
            let tx = pooled_tx.into_consensus();
            if info.is_tx_over_limits(tx.tx(), block_gas_limit, tx_da_limit, block_da_limit) {
                // we can't fit this transaction into the block, so we need to mark it as
                // invalid which also removes all dependent transaction from
                // the iterator before we can continue
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            // if let Some(conditional_options) = pooled_tx.conditional_options() {
            //     todo!("TODO:");
            //     // if validate_conditional_options(conditional_options, &self.client).is_err() {
            //     //     best_txs.mark_invalid(
            //     //         &tx,
            //     //         InvalidPoolTransactionError::Other(Box::new(
            //     //             WorldChainPoolTransactionError::ConditionalValidationFailed(*tx.hash()),
            //     //         )),
            //     //     );
            //     //     invalid_txs.push(*tx.hash());
            //     continue;
            //     // }
            // }

            // // If the transaction is verified, check if it can be added within the verified gas limit
            // if pooled_tx.valid_pbh()
            //     && info.cumulative_gas_used + tx.gas_limit() > verified_gas_limit
            // {
            //     best_txs.mark_invalid(tx.signer(), tx.nonce());
            //     continue;
            // }

            // ensure we still have capacity for this transaction
            if info.cumulative_gas_used + tx.gas_limit() > block_gas_limit {
                // we can't fit this transaction into the block, so we need to mark it as
                // invalid which also removes all dependent transaction from
                // the iterator before we can continue
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            // A sequencer's block should never contain blob or deposit transactions from the pool.
            if tx.is_eip4844() || tx.is_deposit() {
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            // check if the job was cancelled, if so we can exit early
            if self.inner.cancel.is_cancelled() {
                return Ok(Some(()));
            }

            // Configure the environment for the tx.
            let tx_env = self.inner.evm_config.tx_env(tx.tx(), tx.signer());

            let ResultAndState { result, state } = match evm.transact(tx_env) {
                Ok(res) => res,
                Err(err) => {
                    todo!("TODO:")
                    // match err {
                    //     EVMError::Transaction(err) => {
                    //         if matches!(err, InvalidTransaction::NonceTooLow { .. }) {
                    //             // if the nonce is too low, we can skip this transaction
                    //             trace!(target: "payload_builder", %err, ?tx, "skipping nonce too low transaction");
                    //         } else {
                    //             // if the transaction is invalid, we can skip it and all of its
                    //             // descendants
                    //             trace!(target: "payload_builder", %err, ?tx, "skipping invalid transaction and its descendants");
                    //             best_txs.mark_invalid(tx.signer(), tx.nonce());
                    //         }

                    //         continue;
                    //     }

                    //     EVMError::Custom(ref err_str) if err_str == PBH_CALL_TRACER_ERROR => {
                    //         trace!(target: "payload_builder", %err, ?tx, "skipping invalid transaction and its descendants");
                    //         best_txs.mark_invalid(tx.signer(), tx.nonce());
                    //         continue;
                    //     }

                    //     err => {
                    //         // this is an error that we should treat as fatal for this attempt
                    //         return Err(PayloadBuilderError::EvmExecutionError(err));
                    //     }
                    // }
                }
            };

            // commit changes
            evm.db_mut().commit(state);

            let gas_used = result.gas_used();

            // add gas used by the transaction to cumulative gas used, before creating the
            // receipt
            info.cumulative_gas_used += gas_used;
            info.cumulative_da_bytes_used += tx.length() as u64;

            // Push transaction changeset and calculate header bloom filter for receipt.
            info.receipts
                .push(self.build_receipt(info, result, None, &tx));

            // update add to total fees
            let miner_fee = tx
                .effective_tip_per_gas(base_fee)
                .expect("fee is always valid; execution succeeded");
            info.total_fees += U256::from(miner_fee) * U256::from(gas_used);

            // append sender and transaction to the respective lists
            info.executed_senders.push(tx.signer());
            info.executed_transactions.push(tx.into_tx());
        }

        if !invalid_txs.is_empty() {
            pool.remove_transactions(invalid_txs);
        }

        Ok(None)
    }

    /// Constructs a receipt for the given transaction.
    // TODO: Update upstream to expose this function from OpPayloadBuilderCtx and remove this
    pub fn build_receipt(
        &self,
        info: &ExecutionInfo<N>,
        result: ExecutionResult,
        deposit_nonce: Option<u64>,
        tx: &N::SignedTx,
    ) -> N::Receipt {
        match self.inner.receipt_builder.build_receipt(ReceiptBuilderCtx {
            tx,
            result,
            cumulative_gas_used: info.cumulative_gas_used,
        }) {
            Ok(receipt) => receipt,
            Err(ctx) => {
                let receipt = alloy_consensus::Receipt {
                    // Success flag was added in `EIP-658: Embedding transaction status code
                    // in receipts`.
                    status: Eip658Value::Eip658(ctx.result.is_success()),
                    cumulative_gas_used: ctx.cumulative_gas_used,
                    logs: ctx.result.into_logs(),
                };

                self.inner
                    .receipt_builder
                    .build_deposit_receipt(OpDepositReceipt {
                        inner: receipt,
                        deposit_nonce,
                        // The deposit receipt version was introduced in Canyon to indicate an
                        // update to how receipt hashes should be computed
                        // when set. The state transition process ensures
                        // this is only set for post-Canyon deposit
                        // transactions.
                        deposit_receipt_version: self.inner.is_canyon_active().then_some(1),
                    })
            }
        }
    }
}
