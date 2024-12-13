use alloy_consensus::EMPTY_OMMER_ROOT_HASH;
use alloy_eips::merge::BEACON_NONCE;
use alloy_rpc_types_debug::ExecutionWitness;
use reth::api::PayloadBuilderError;
use reth::builder::components::PayloadServiceBuilder;
use reth::builder::{BuilderContext, FullNodeTypes, NodeTypesWithEngine, PayloadBuilderConfig};
use reth::chainspec::EthereumHardforks;
use reth::payload::PayloadBuilderAttributes;
use reth::payload::{PayloadBuilderHandle, PayloadBuilderService};
use reth::revm::database::StateProviderDatabase;
use reth::revm::db::states::bundle_state::BundleRetention;
use reth::revm::witness::ExecutionWitnessRecord;
use reth::revm::DatabaseCommit;
use reth::revm::State;
use reth::transaction_pool::{BestTransactionsAttributes, PoolTransaction, TransactionPool};
use reth_basic_payload_builder::{
    commit_withdrawals, is_better_payload, BasicPayloadJobGenerator,
    BasicPayloadJobGeneratorConfig, BuildArguments, BuildOutcome, BuildOutcomeKind,
    MissingPayloadBehaviour, PayloadBuilder, PayloadConfig,
};
use reth_chain_state::ExecutedBlock;
use reth_db::DatabaseEnv;
use reth_evm::system_calls::SystemCaller;
use reth_evm::{ConfigureEvm, NextBlockEnvAttributes};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_consensus::calculate_receipt_root_no_memo_optimism;
use reth_optimism_node::{OpBuiltPayload, OpPayloadBuilder, OpPayloadBuilderAttributes};
use reth_optimism_payload_builder::builder::{
    ExecutedPayload, OpBuilder, OpPayloadBuilderCtx, OpPayloadTransactions,
};
use reth_optimism_payload_builder::config::OpBuilderConfig;
use reth_optimism_payload_builder::OpPayloadAttributes;
use reth_primitives::{proofs, BlockBody, BlockExt, SealedHeader, TransactionSigned};
use reth_primitives::{Block, Header, Receipt, TxType};
use reth_provider::{
    BlockReaderIdExt, CanonStateSubscriptions, ChainSpecProvider, ExecutionOutcome,
    HashedPostStateProvider, ProviderError, StateProofProvider, StateProviderFactory,
    StateRootProvider,
};
use reth_transaction_pool::{noop::NoopTransactionPool, pool::BestPayloadTransactions};
use reth_trie::HashedPostState;
use revm::Database;
use revm_primitives::calc_excess_blob_gas;
use revm_primitives::{
    BlockEnv, CfgEnvWithHandlerCfg, EVMError, EnvWithHandlerCfg, InvalidTransaction,
    ResultAndState, U256,
};
use std::sync::Arc;
use tracing::{debug, trace, warn};

use world_chain_builder_pool::noop::NoopWorldChainTransactionPool;
use world_chain_builder_pool::tx::WorldChainPoolTransaction;
use world_chain_builder_rpc::bundle::validate_conditional_options;

/// World Chain payload builder
#[derive(Debug)]
pub struct WorldChainPayloadBuilder<EvmConfig, Tx = ()> {
    pub inner: OpPayloadBuilder<EvmConfig, Tx>,
    pub verified_blockspace_capacity: u8,
}

impl<EvmConfig> WorldChainPayloadBuilder<EvmConfig, ()>
where
    EvmConfig: ConfigureEvm<Header = Header>,
{
    pub fn new(evm_config: EvmConfig, verified_blockspace_capacity: u8) -> Self {
        Self::with_builder_config(evm_config, Default::default(), verified_blockspace_capacity)
    }

    pub const fn with_builder_config(
        evm_config: EvmConfig,
        builder_config: OpBuilderConfig,
        verified_blockspace_capacity: u8,
    ) -> Self {
        let inner = OpPayloadBuilder::with_builder_config(evm_config, builder_config);

        Self {
            inner,
            verified_blockspace_capacity,
        }
    }
}

impl<EvmConfig> WorldChainPayloadBuilder<EvmConfig> {
    /// Sets the rollup's compute pending block configuration option.
    pub const fn set_compute_pending_block(mut self, compute_pending_block: bool) -> Self {
        self.inner.compute_pending_block = compute_pending_block;
        self
    }

    pub fn with_transactions<T: OpPayloadTransactions>(
        self,
        best_transactions: T,
    ) -> WorldChainPayloadBuilder<EvmConfig, T> {
        let Self {
            inner,
            verified_blockspace_capacity,
        } = self;

        let OpPayloadBuilder {
            compute_pending_block,
            evm_config,
            config,
            ..
        } = inner;

        WorldChainPayloadBuilder {
            inner: OpPayloadBuilder {
                compute_pending_block,
                evm_config,
                best_transactions,
                config,
            },
            verified_blockspace_capacity,
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

impl<EvmConfig, Txs> WorldChainPayloadBuilder<EvmConfig, Txs>
where
    EvmConfig: ConfigureEvm<Header = Header, Transaction = TransactionSigned>,
    Txs: OpPayloadTransactions,
{
    /// Constructs an Optimism payload from the transactions sent via the
    /// Payload attributes by the sequencer. If the `no_tx_pool` argument is passed in
    /// the payload attributes, the transaction pool will be ignored and the only transactions
    /// included in the payload will be those sent through the attributes.
    ///
    /// Given build arguments including an Optimism client, transaction pool,
    /// and configuration, this function creates a transaction payload. Returns
    /// a result indicating success with the payload or an error in case of failure.
    fn build_payload<Client, Pool>(
        &self,
        args: BuildArguments<Pool, Client, OpPayloadBuilderAttributes, OpBuiltPayload>,
    ) -> Result<BuildOutcome<OpBuiltPayload>, PayloadBuilderError>
    where
        Client: StateProviderFactory + ChainSpecProvider<ChainSpec = OpChainSpec>,
        Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TransactionSigned>>,
    {
        let (initialized_cfg, initialized_block_env) = self
            .cfg_and_block_env(&args.config.attributes, &args.config.parent_header)
            .map_err(PayloadBuilderError::other)?;

        let BuildArguments {
            client,
            pool,
            mut cached_reads,
            config,
            cancel,
            best_payload,
        } = args;

        let ctx = OpPayloadBuilderCtx {
            evm_config: self.inner.evm_config.clone(),
            chain_spec: client.chain_spec(),
            config,
            initialized_cfg,
            initialized_block_env,
            cancel,
            best_payload,
        };

        let builder = WorldChainBuilder {
            pool,
            best: self.inner.best_transactions.clone(),
        };

        let state_provider = client.state_by_block_hash(ctx.parent().hash())?;
        let state = StateProviderDatabase::new(state_provider);

        if ctx.attributes().no_tx_pool {
            let db = State::builder()
                .with_database(state)
                .with_bundle_update()
                .build();
            builder.build(db, ctx)
        } else {
            // sequencer mode we can reuse cachedreads from previous runs
            let db = State::builder()
                .with_database(cached_reads.as_db_mut(state))
                .with_bundle_update()
                .build();
            builder.build(db, ctx)
        }
        .map(|out| out.with_cached_reads(cached_reads))
    }
}

impl<EvmConfig, Txs> WorldChainPayloadBuilder<EvmConfig, Txs>
where
    EvmConfig: ConfigureEvm<Header = Header, Transaction = TransactionSigned>,
{
    /// Returns the configured [`CfgEnvWithHandlerCfg`] and [`BlockEnv`] for the targeted payload
    /// (that has the `parent` as its parent).
    pub fn cfg_and_block_env(
        &self,
        attributes: &OpPayloadBuilderAttributes,
        parent: &Header,
    ) -> Result<(CfgEnvWithHandlerCfg, BlockEnv), EvmConfig::Error> {
        let next_attributes = NextBlockEnvAttributes {
            timestamp: attributes.timestamp(),
            suggested_fee_recipient: attributes.suggested_fee_recipient(),
            prev_randao: attributes.prev_randao(),
            // gas_limit: attributes.gas_limit.unwrap_or(parent.gas_limit),
        };
        self.inner
            .evm_config
            .next_cfg_and_block_env(parent, next_attributes)
    }

    /// Computes the witness for the payload.
    pub fn payload_witness<Client>(
        &self,
        client: &Client,
        parent: SealedHeader,
        attributes: OpPayloadAttributes,
    ) -> Result<ExecutionWitness, PayloadBuilderError>
    where
        Client: StateProviderFactory + ChainSpecProvider<ChainSpec = OpChainSpec>,
    {
        let attributes = OpPayloadBuilderAttributes::try_new(parent.hash(), attributes, 3)
            .map_err(PayloadBuilderError::other)?;

        let (initialized_cfg, initialized_block_env) = self
            .cfg_and_block_env(&attributes, &parent)
            .map_err(PayloadBuilderError::other)?;

        let config = PayloadConfig {
            parent_header: Arc::new(parent),
            attributes,
            extra_data: Default::default(),
        };

        let ctx = OpPayloadBuilderCtx {
            evm_config: self.inner.evm_config.clone(),
            chain_spec: client.chain_spec(),
            config,
            initialized_cfg,
            initialized_block_env,
            cancel: Default::default(),
            best_payload: Default::default(),
        };

        let state_provider = client.state_by_block_hash(ctx.parent().hash())?;
        let state = StateProviderDatabase::new(state_provider);
        let mut state = State::builder()
            .with_database(state)
            .with_bundle_update()
            .build();

        let builder = WorldChainBuilder {
            pool: NoopTransactionPool::default(),
            best: (),
        };
        builder.witness(&mut state, &ctx)
    }
}

impl<Pool, Txs> WorldChainBuilder<Pool, Txs>
where
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TransactionSigned>>,
    Txs: OpPayloadTransactions,
{
    /// Executes the payload and returns the outcome.
    pub fn execute<EvmConfig, DB>(
        self,
        state: &mut State<DB>,
        ctx: &OpPayloadBuilderCtx<EvmConfig>,
    ) -> Result<BuildOutcomeKind<ExecutedPayload>, PayloadBuilderError>
    where
        EvmConfig: ConfigureEvm<Header = Header, Transaction = TransactionSigned>,
        DB: Database<Error = ProviderError>,
    {
        let Self { pool, best } = self;
        debug!(target: "payload_builder", id=%ctx.payload_id(), parent_header = ?ctx.parent().hash(), parent_number = ctx.parent().number, "building new payload");

        // 1. apply eip-4788 pre block contract call
        ctx.apply_pre_beacon_root_contract_call(state)?;

        // 2. ensure create2deployer is force deployed
        ctx.ensure_create2_deployer(state)?;

        // 3. execute sequencer transactions
        let mut info = ctx.execute_sequencer_transactions(state)?;

        // 4. if mem pool transactions are requested we execute them
        if !ctx.attributes().no_tx_pool {
            //TODO: build pbh payload

            let best_txs = best.best_transactions(pool, ctx.best_transaction_attributes());
            if ctx
                .execute_best_transactions::<_, Pool>(&mut info, state, best_txs)?
                .is_some()
            {
                return Ok(BuildOutcomeKind::Cancelled);
            }

            // check if the new payload is even more valuable
            if !ctx.is_better_payload(info.total_fees) {
                // can skip building the block
                return Ok(BuildOutcomeKind::Aborted {
                    fees: info.total_fees,
                });
            }
        }

        let withdrawals_root = ctx.commit_withdrawals(state)?;

        // merge all transitions into bundle state, this would apply the withdrawal balance changes
        // and 4788 contract call
        state.merge_transitions(BundleRetention::Reverts);

        Ok(BuildOutcomeKind::Better {
            payload: ExecutedPayload {
                info,
                withdrawals_root,
            },
        })
    }

    // TODO:
    /// Builds the payload on top of the state.
    pub fn build<EvmConfig, DB, P>(
        self,
        mut state: State<DB>,
        ctx: OpPayloadBuilderCtx<EvmConfig>,
    ) -> Result<BuildOutcomeKind<OpBuiltPayload>, PayloadBuilderError>
    where
        EvmConfig: ConfigureEvm<Header = Header, Transaction = TransactionSigned>,
        DB: Database<Error = ProviderError> + AsRef<P>,
        P: StateRootProvider + HashedPostStateProvider,
    {
        let ExecutedPayload {
            info,
            withdrawals_root,
        } = match self.execute(&mut state, &ctx)? {
            BuildOutcomeKind::Better { payload } | BuildOutcomeKind::Freeze(payload) => payload,
            BuildOutcomeKind::Cancelled => return Ok(BuildOutcomeKind::Cancelled),
            BuildOutcomeKind::Aborted { fees } => return Ok(BuildOutcomeKind::Aborted { fees }),
        };

        let block_number = ctx.block_number();
        let execution_outcome = ExecutionOutcome::new(
            state.take_bundle(),
            vec![info.receipts].into(),
            block_number,
            Vec::new(),
        );
        let receipts_root = execution_outcome
            .generic_receipts_root_slow(block_number, |receipts| {
                calculate_receipt_root_no_memo_optimism(
                    receipts,
                    &ctx.chain_spec,
                    ctx.attributes().timestamp(),
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
                    parent_header=%ctx.parent().hash(),
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
        let (excess_blob_gas, blob_gas_used) = ctx.blob_fields();
        let extra_data = ctx.extra_data()?;

        let header = Header {
            parent_hash: ctx.parent().hash(),
            ommers_hash: EMPTY_OMMER_ROOT_HASH,
            beneficiary: ctx.initialized_block_env.coinbase,
            state_root,
            transactions_root,
            receipts_root,
            withdrawals_root,
            logs_bloom,
            timestamp: ctx.attributes().payload_attributes.timestamp,
            mix_hash: ctx.attributes().payload_attributes.prev_randao,
            nonce: BEACON_NONCE.into(),
            base_fee_per_gas: Some(ctx.base_fee()),
            number: ctx.parent().number + 1,
            gas_limit: ctx.block_gas_limit(),
            difficulty: U256::ZERO,
            gas_used: info.cumulative_gas_used,
            extra_data,
            parent_beacon_block_root: ctx.attributes().payload_attributes.parent_beacon_block_root,
            blob_gas_used,
            excess_blob_gas,
            requests_hash: None,
            target_blobs_per_block: None,
        };

        // seal the block
        let block = Block {
            header,
            body: BlockBody {
                transactions: info.executed_transactions,
                ommers: vec![],
                withdrawals: ctx.withdrawals().cloned(),
            },
        };

        let sealed_block = Arc::new(block.seal_slow());
        debug!(target: "payload_builder", id=%ctx.attributes().payload_id(), sealed_block_header = ?sealed_block.header, "sealed built block");

        // create the executed block data
        let executed = ExecutedBlock {
            block: sealed_block.clone(),
            senders: Arc::new(info.executed_senders),
            execution_output: Arc::new(execution_outcome),
            hashed_state: Arc::new(hashed_state),
            trie: Arc::new(trie_output),
        };

        let no_tx_pool = ctx.attributes().no_tx_pool;

        let payload = OpBuiltPayload::new(
            ctx.payload_id(),
            sealed_block,
            info.total_fees,
            ctx.chain_spec.clone(),
            ctx.config.attributes,
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
    pub fn witness<EvmConfig, DB, P>(
        self,
        state: &mut State<DB>,
        ctx: &OpPayloadBuilderCtx<EvmConfig>,
    ) -> Result<ExecutionWitness, PayloadBuilderError>
    where
        EvmConfig: ConfigureEvm<Header = Header, Transaction = TransactionSigned>,
        DB: Database<Error = ProviderError> + AsRef<P>,
        P: StateProofProvider,
    {
        let _ = self.execute(state, ctx)?;
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
#[derive(Debug)]
pub struct WorldChainBuilder<Pool, Txs> {
    /// The transaction pool
    pool: Pool,
    /// Yields the best transaction to include if transactions from the mempool are allowed.
    best: Txs,
}
