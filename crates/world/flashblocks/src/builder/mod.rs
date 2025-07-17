use crate::payload_builder_ctx::{PayloadBuilderCtx, PayloadBuilderCtxBuilder};
use alloy_consensus::{
    constants::EMPTY_WITHDRAWALS, proofs, BlockBody, BlockHeader, Header, EMPTY_OMMER_ROOT_HASH,
};
use alloy_eips::{eip7685::EMPTY_REQUESTS_HASH, merge::BEACON_NONCE};
use alloy_primitives::{map::foldhash::HashMap, Address, Bytes, B256, U256};
use op_alloy_consensus::OpTxEnvelope;
use reth::{
    api::{Block, PayloadBuilderAttributes, PayloadBuilderError},
    chainspec::EthChainSpec,
    revm::{cancelled::CancelOnDrop, database::StateProviderDatabase, Database, State},
};
use rollup_boost::{
    ed25519_dalek::{SigningKey, VerifyingKey},
    Authorization, Authorized,
};
use rollup_boost::{
    ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1, FlashblocksP2PMsg,
    FlashblocksPayloadV1,
};

use alloy_eips::Encodable2718;
use reth_basic_payload_builder::{BuildArguments, BuildOutcome, BuildOutcomeKind};
use reth_basic_payload_builder::{MissingPayloadBehaviour, PayloadBuilder, PayloadConfig};
use reth_evm::{execute::BlockBuilder, ConfigureEvm};
use reth_optimism_consensus::{calculate_receipt_root_no_memo_optimism, isthmus};

use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{txpool::OpPooledTx, OpEvmConfig, OpNextBlockEnvAttributes};
use reth_optimism_payload_builder::config::OpBuilderConfig;
use reth_optimism_payload_builder::{
    builder::OpPayloadTransactions,
    payload::{OpBuiltPayload, OpPayloadBuilderAttributes},
};
use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
use reth_payload_util::{NoopPayloadTransactions, PayloadTransactions};
use reth_provider::{
    ChainSpecProvider, ExecutionOutcome, HashedPostStateProvider, ProviderError, StateProvider,
    StateProviderFactory, StateRootProvider, StorageRootProvider,
};

use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction, TransactionPool};
use revm::database::{states::bundle_state::BundleRetention, BundleState};
use serde_json::json;
use std::{fmt::Debug, sync::Arc};
use tokio::{sync::mpsc, time::Instant};
use tracing::{debug, error, info, span, warn};

mod retaining_payload_txs;

/// Flashblocks Paylod builder
///
/// A payload builder
#[derive(Debug)]
pub struct FlashblocksPayloadBuilder<Pool, Client, CtxBuilder, Txs = ()> {
    /// The type responsible for creating the evm.
    pub evm_config: OpEvmConfig,
    /// Transaction pool.
    pub pool: Pool,
    /// Node client.
    pub client: Client,
    /// Settings for the builder, e.g. DA settings.
    pub config: OpBuilderConfig,
    /// The type responsible for yielding the best transactions for the payload if mempool
    /// transactions are allowed.
    pub best_transactions: Txs,
    /// Block time in milliseconds
    pub block_time: u64,
    /// Flashblock interval in milliseconds
    pub flashblock_interval: u64,
    pub ctx_builder: CtxBuilder,
    /// Channel for publishing messages
    pub publish_tx: mpsc::UnboundedSender<FlashblocksP2PMsg>,
    pub authorizer_vk: Option<VerifyingKey>,
    pub builder_sk: SigningKey,
}

// TODO: This manual impl is required because we can't require PayloadBuilderCtx
//       to be Clone, because OpPayloadBuilderCtx is not Clone.
//       The workaround is to put ctx in `Arc` and not have to depend on it being Clone
impl<Pool, Client, CtxBuilder, Txs> Clone
    for FlashblocksPayloadBuilder<Pool, Client, CtxBuilder, Txs>
where
    Pool: Clone,
    Client: Clone,
    Txs: Clone,
    CtxBuilder: Clone,
{
    fn clone(&self) -> Self {
        Self {
            evm_config: self.evm_config.clone(),
            pool: self.pool.clone(),
            client: self.client.clone(),
            config: self.config.clone(),
            best_transactions: self.best_transactions.clone(),
            block_time: self.block_time,
            flashblock_interval: self.flashblock_interval,
            ctx_builder: self.ctx_builder.clone(),
            publish_tx: self.publish_tx.clone(),
            authorizer_vk: self.authorizer_vk,
            builder_sk: self.builder_sk.clone(),
        }
    }
}

impl<Pool, Client, CtxBuilder, Txs> FlashblocksPayloadBuilder<Pool, Client, CtxBuilder, Txs>
where
    Txs: OpPayloadTransactions<Pool::Transaction>,
    Pool: TransactionPool<Transaction: OpPooledTx<Consensus = OpTxEnvelope>>,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec = OpChainSpec>,
    CtxBuilder: PayloadBuilderCtxBuilder<OpChainSpec, Pool::Transaction>,
{
    /// Constructs an Optimism payload from the transactions sent via the
    /// Payload attributes by the sequencer. If the `no_tx_pool` argument is passed in
    /// the payload attributes, the transaction pool will be ignored and the only transactions
    /// included in the payload will be those sent through the attributes.
    ///
    /// Given build arguments including an Optimism client, transaction pool,
    /// and configuration, this function creates a transaction payload. Returns
    /// a result indicating success with the payload or an error in case of failure.
    fn build_payload<'a, T>(
        &self,
        args: BuildArguments<OpPayloadBuilderAttributes<OpTxEnvelope>, OpBuiltPayload>,
        best: impl Fn(BestTransactionsAttributes) -> T + Send + Sync + 'a,
    ) -> Result<BuildOutcome<OpBuiltPayload>, PayloadBuilderError>
    where
        T: PayloadTransactions<Transaction = <Pool as TransactionPool>::Transaction>,
    {
        let BuildArguments {
            mut cached_reads,
            config,
            cancel,
            best_payload,
        } = args;

        let ctx = self.ctx_builder.build::<Txs>(
            self.evm_config.clone(),
            self.config.da_config.clone(),
            self.client.chain_spec(),
            config,
            &cancel,
            best_payload,
        );

        let builder = FlashblockBuilder::new(
            best,
            self.publish_tx.clone(),
            // TODO: figure out how to get the authorization from the FCU
            None,
            self.builder_sk.clone(),
            self.block_time,
            self.flashblock_interval,
            cancel.clone(),
        );

        let state_provider = self.client.state_by_block_hash(ctx.parent().hash())?;
        let state = StateProviderDatabase::new(&state_provider);

        if ctx.attributes().no_tx_pool {
            builder.build(state, &state_provider, &ctx)
        } else {
            // sequencer mode we can reuse cachedreads from previous runs
            builder.build(cached_reads.as_db_mut(state), &state_provider, &ctx)
        }
        .map(|out| out.with_cached_reads(cached_reads))
    }
}

impl<Pool, Client, CtxBuilder, Txs> PayloadBuilder
    for FlashblocksPayloadBuilder<Pool, Client, CtxBuilder, Txs>
where
    Client: Clone + StateProviderFactory + ChainSpecProvider<ChainSpec = OpChainSpec>,
    Pool: TransactionPool<Transaction: OpPooledTx<Consensus = OpTxEnvelope>>,
    CtxBuilder: PayloadBuilderCtxBuilder<Client::ChainSpec, Pool::Transaction>,
    Txs: OpPayloadTransactions<Pool::Transaction>,
{
    type Attributes = OpPayloadBuilderAttributes<OpTxEnvelope>;
    type BuiltPayload = OpBuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError> {
        self.build_payload(args, |attrs| {
            self.best_transactions
                .best_transactions(self.pool.clone(), attrs)
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

    fn build_empty_payload(
        &self,
        config: PayloadConfig<Self::Attributes, alloy_consensus::Header>,
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
pub struct FlashblockBuilder<'a, Txs>
where
    Txs: PayloadTransactions,
{
    /// Yields the best transaction to include if transactions from the mempool are allowed.
    best: Box<dyn Fn(BestTransactionsAttributes) -> Txs + 'a>,

    /// Channel sender for publishing messages
    pub tx: mpsc::UnboundedSender<FlashblocksP2PMsg>,
    pub authorization: Option<Authorization>,
    pub builder_sk: SigningKey,
    pub block_time: u64,
    pub flashblock_interval: u64,
    pub cancel: CancelOnDrop,
}

impl<'a, Txs> FlashblockBuilder<'a, Txs>
where
    Txs: PayloadTransactions,
{
    /// Creates a new [`OpBuilder`].
    pub fn new(
        best: impl Fn(BestTransactionsAttributes) -> Txs + Send + Sync + 'a,
        tx: mpsc::UnboundedSender<FlashblocksP2PMsg>,
        authorization: Option<Authorization>,
        builder_sk: SigningKey,
        block_time: u64,
        flashblock_interval: u64,
        cancel: CancelOnDrop,
    ) -> Self {
        Self {
            best: Box::new(best),
            authorization,
            builder_sk,
            tx,
            block_time,
            flashblock_interval,
            cancel,
        }
    }
}

impl<'a, Txs> FlashblockBuilder<'_, Txs>
where
    Txs: PayloadTransactions,
{
    /// Builds the payload on top of the state.
    pub fn build<Ctx, Tx>(
        self,
        db: impl Database<Error = ProviderError> + 'a,
        state_provider: impl StateProvider + Clone,
        ctx: &Ctx,
    ) -> Result<BuildOutcomeKind<OpBuiltPayload>, PayloadBuilderError>
    where
        Tx: PoolTransaction<Consensus = OpTransactionSigned> + OpPooledTx,
        Txs: PayloadTransactions<Transaction = Tx>,
        Ctx: PayloadBuilderCtx<Evm = OpEvmConfig, Transaction = Tx, ChainSpec = OpChainSpec>,
    {
        let span = span!(
            tracing::Level::INFO,
            "flashblock_builder",
            id = %ctx.payload_id(),
        );

        let _enter = span.enter();
        debug!(target: "payload_builder", id=%ctx.payload_id(), "building new payload");

        // 1. Setup relevant variables
        let mut flashblock_idx = 0;
        let gas_limit = ctx.attributes().gas_limit.unwrap_or(ctx.parent().gas_limit);

        let attributes = OpNextBlockEnvAttributes {
            timestamp: ctx.attributes().timestamp(),
            suggested_fee_recipient: ctx.attributes().suggested_fee_recipient(),
            prev_randao: ctx.attributes().prev_randao(),
            gas_limit: ctx.attributes().gas_limit.unwrap_or(ctx.parent().gas_limit),
            parent_beacon_block_root: ctx.attributes().parent_beacon_block_root(),
            extra_data: if ctx
                .spec()
                .is_holocene_active_at_timestamp(ctx.attributes().timestamp())
            {
                ctx.attributes()
                    .get_holocene_extra_data(
                        ctx.spec()
                            .base_fee_params_at_timestamp(ctx.attributes().timestamp()),
                    )
                    .map_err(PayloadBuilderError::other)?
            } else {
                Default::default()
            },
        };

        let state = StateProviderDatabase::new(&state_provider);
        let mut db = State::builder()
            .with_database(state)
            .with_bundle_update()
            .build();

        ctx.evm_config()
            .builder_for_next_block(&mut db, ctx.parent(), attributes.clone())
            .map_err(PayloadBuilderError::other)?
            .apply_pre_execution_changes()?;

        let mut info = ctx
            .execute_sequencer_transactions(&mut db)
            .map_err(PayloadBuilderError::other)?;

        let (payload, flashblock_payload, mut bundle_state) =
            Self::build_block(db, ctx, &mut info)?;

        let authorization = self.authorization.clone().unwrap_or_else(|| {
            // if no authorization is provided, we create one using the builder's signing key
            Authorization::new(
                flashblock_payload.payload_id,
                self.block_time,
                &self.builder_sk,
                self.builder_sk.verifying_key(),
            )
        });
        let authorized =
            Authorized::new(&self.builder_sk, authorization.clone(), flashblock_payload);
        let p2p_msg = FlashblocksP2PMsg::FlashblocksPayloadV1(authorized);
        if let Err(err) = self.tx.send(p2p_msg) {
            error!(target: "payload_builder", %err, "failed to send flashblock payload");
        };

        // 2. Build the flashblocks
        let total_flashbblocks = self.block_time as usize / self.flashblock_interval as usize;
        let (tx, mut rx) = tokio::sync::mpsc::channel(total_flashbblocks);

        // spawn a task to schedule when the next flashblock job should be started/cancelled
        self.spawn_flashblock_job_manager(tx);

        loop {
            debug!(target: "payload_builder", id=%ctx.attributes().payload_id(), "building flashblock {flashblock_idx}");

            let mut state = reth::revm::State::builder()
                .with_database(StateProviderDatabase::new(&state_provider))
                .with_bundle_update()
                .with_bundle_prestate(bundle_state)
                .build();

            let notify = tokio::task::block_in_place(|| rx.blocking_recv());

            match notify {
                Some(()) => {
                    let best_txns =
                        (*self.best)(ctx.best_transaction_attributes(ctx.evm_env().block_env()));

                    let evm = ctx.evm_config().evm_for_block(&mut state, ctx.parent());

                    {
                        let Some(()) =
                            ctx.execute_best_transactions(&mut info, evm, best_txns, gas_limit)?
                        else {
                            debug!(target: "payload_builder", id=%ctx.attributes().payload_id(), "no more transactions to include in flashblock {flashblock_idx}");
                            break;
                        };
                    }

                    let (payload, flashblock_payload, new_bundle) =
                        Self::build_block(state, ctx, &mut info)
                            .map_err(PayloadBuilderError::other)?;

                    info!(
                        target: "payload_builder",
                        id=%ctx.attributes().payload_id(),
                        transactions = payload.block().body().transactions.len()
                    );

                    let authorized = Authorized::new(
                        &self.builder_sk,
                        authorization.clone(),
                        flashblock_payload,
                    );
                    let p2p_msg = FlashblocksP2PMsg::FlashblocksPayloadV1(authorized);

                    if let Err(err) = self.tx.send(p2p_msg) {
                        error!(target: "payload_builder", %err, "failed to send flashblock payload");
                    }

                    bundle_state = new_bundle;
                }
                // tx was dropped, resolve the most recent payload
                None => {
                    debug!(target: "payload_builder", id=%ctx.attributes().payload_id(), "no more flashblocks to build, resolving payload");
                    if flashblock_idx == 0 {
                        // if no flashblocks were built, we can return the payload as is
                        return Ok(BuildOutcomeKind::Freeze(payload));
                    }

                    break;
                }
            }

            flashblock_idx += 1;
        }

        if ctx.attributes().no_tx_pool {
            // if `no_tx_pool` is set only transactions from the payload attributes will be included
            // in the payload. In other words, the payload is deterministic and we can
            // freeze it once we've successfully built it.
            Ok(BuildOutcomeKind::Freeze(payload))
        } else {
            info!(target: "payload_builder", id=%ctx.attributes().payload_id(), payload=?payload);

            Ok(BuildOutcomeKind::Better { payload })
        }
    }

    fn build_block<Ctx, DB, DP, Tx>(
        mut state: State<DB>,
        ctx: &Ctx,
        info: &mut ExecutionInfo<FlashblocksExecutionMetadata>,
    ) -> Result<(OpBuiltPayload, FlashblocksPayloadV1, BundleState), PayloadBuilderError>
    where
        Ctx: PayloadBuilderCtx<Evm = OpEvmConfig, Transaction = Tx, ChainSpec = OpChainSpec>,
        DB: Database<Error = ProviderError> + AsRef<DP>,
        DP: StateRootProvider + HashedPostStateProvider + StorageRootProvider,
    {
        let block_number = ctx.parent().number + 1;
        state.merge_transitions(BundleRetention::Reverts);

        let bundle = state.take_bundle();
        let execution_outcome = ExecutionOutcome::new(
            bundle.clone(),
            vec![info.receipts.clone()],
            ctx.parent().number + 1,
            vec![], // TODO:
        );

        let receipts_root = execution_outcome
            .generic_receipts_root_slow(block_number, |receipts| {
                calculate_receipt_root_no_memo_optimism(
                    receipts,
                    ctx.spec(),
                    ctx.attributes().timestamp(),
                )
            })
            .ok_or(PayloadBuilderError::Other(
                "Failed to calculate receipts root".into(),
            ))?;

        let logs_bloom =
            execution_outcome
                .block_logs_bloom(block_number)
                .ok_or(PayloadBuilderError::Other(
                    "Failed to calculate logs bloom".into(),
                ))?;

        let state_provider = state.database.as_ref();
        let hashed_state = state_provider.hashed_post_state(execution_outcome.state());
        let (state_root, _trie_output) = {
            state
                .database
                .as_ref()
                .state_root_with_updates(hashed_state.clone())
                .inspect_err(|err| {
                    warn!(target: "payload_builder",
                    parent_header=%ctx.parent().hash(),
                        %err,
                        "failed to calculate state root for payload"
                    );
                })?
        };

        let mut requests_hash = None;
        let withdrawals_root = if ctx
            .spec()
            .is_isthmus_active_at_timestamp(ctx.attributes().timestamp())
        {
            // always empty requests hash post isthmus
            requests_hash = Some(EMPTY_REQUESTS_HASH);

            // withdrawals root field in block header is used for storage root of L2 predeploy
            // `l2tol1-message-passer`
            Some(
                isthmus::withdrawals_root(execution_outcome.state(), state.database.as_ref())
                    .map_err(PayloadBuilderError::other)?,
            )
        } else if ctx
            .spec()
            .is_canyon_active_at_timestamp(ctx.attributes().timestamp())
        {
            Some(EMPTY_WITHDRAWALS)
        } else {
            None
        };

        // create the block header
        let transactions_root = proofs::calculate_transaction_root(&info.executed_transactions);

        let block_env = ctx.evm_config().evm_env(ctx.parent()).block_env;

        let header = Header {
            parent_hash: ctx.parent().hash(),
            ommers_hash: EMPTY_OMMER_ROOT_HASH,
            beneficiary: block_env.beneficiary,
            state_root,
            transactions_root,
            receipts_root,
            withdrawals_root,
            logs_bloom,
            timestamp: ctx.attributes().payload_attributes.timestamp,
            mix_hash: ctx.attributes().payload_attributes.prev_randao,
            nonce: BEACON_NONCE.into(),
            base_fee_per_gas: Some(ctx.evm_env().block_env().basefee),
            number: ctx.parent().number + 1,
            gas_limit: ctx.attributes().gas_limit.unwrap_or(ctx.parent().gas_limit),
            difficulty: U256::ZERO,
            gas_used: info.cumulative_gas_used,
            extra_data: Bytes::default(),
            parent_beacon_block_root: ctx.attributes().payload_attributes.parent_beacon_block_root,
            blob_gas_used: None, // TODO: double check
            excess_blob_gas: None,
            requests_hash,
        };

        // seal the block
        let block = alloy_consensus::Block::<OpTransactionSigned>::new(
            header,
            BlockBody {
                transactions: info.executed_transactions.clone(),
                ommers: vec![],
                withdrawals: ctx.withdrawals().cloned(),
            },
        );

        let sealed_block = Arc::new(block.seal_slow());
        debug!(target: "payload_builder", ?sealed_block, "sealed built block");

        let block_hash = sealed_block.hash();

        // pick the new transactions from the info field and update the last flashblock index
        let new_transactions =
            info.executed_transactions[info.extra.last_flashblock_offset..].to_vec();

        let new_transactions_encoded = new_transactions
            .clone()
            .into_iter()
            .map(|tx| tx.encoded_2718().into())
            .collect::<Vec<_>>();

        let new_receipts = info.receipts[info.extra.last_flashblock_offset..].to_vec();
        info.extra.last_flashblock_offset = info.executed_transactions.len();

        let receipts_with_hash = new_transactions
            .iter()
            .zip(new_receipts.iter())
            .map(|(tx, receipt)| (tx.tx_hash(), receipt.clone()))
            .collect::<HashMap<B256, OpReceipt>>();
        let new_account_balances = bundle
            .state
            .iter()
            .filter_map(|(address, account)| {
                account.info.as_ref().map(|info| (*address, info.balance))
            })
            .collect::<HashMap<Address, U256>>();

        let metadata = json! ({
            "receipts": receipts_with_hash,
            "new_account_balances": new_account_balances,
            "block_number": ctx.parent().number + 1,
        });

        // Prepare the flashblocks message
        let fb_payload = FlashblocksPayloadV1 {
            payload_id: ctx.payload_id(),
            index: 0,
            base: Some(ExecutionPayloadBaseV1 {
                parent_beacon_block_root: ctx
                    .attributes()
                    .payload_attributes
                    .parent_beacon_block_root
                    .unwrap(),
                parent_hash: ctx.parent().hash(),
                fee_recipient: ctx.attributes().suggested_fee_recipient(),
                prev_randao: ctx.attributes().payload_attributes.prev_randao,
                block_number: ctx.parent().number + 1,
                gas_limit: ctx.attributes().gas_limit.unwrap_or(ctx.parent().gas_limit),
                timestamp: ctx.attributes().payload_attributes.timestamp,
                extra_data: ctx
                    .attributes()
                    .get_holocene_extra_data(ctx.spec().base_fee_params_at_timestamp(
                        ctx.attributes().payload_attributes.timestamp,
                    ))
                    .unwrap_or_default(),
                base_fee_per_gas: U256::from(ctx.evm_env().block_env().basefee),
            }),
            diff: ExecutionPayloadFlashblockDeltaV1 {
                state_root,
                receipts_root,
                logs_bloom,
                gas_used: info.cumulative_gas_used,
                block_hash,
                transactions: new_transactions_encoded,
                withdrawals: ctx.withdrawals().cloned().unwrap_or_default().to_vec(),
                withdrawals_root: withdrawals_root.unwrap_or_default(),
            },
            metadata: serde_json::to_value(&metadata).unwrap_or_default(),
        };

        Ok((
            OpBuiltPayload::new(
                ctx.payload_id(),
                sealed_block,
                info.total_fees,
                // This must be set to NONE for now because we are doing merge transitions on every flashblock
                // when it should only happen once per block, thus, it returns a confusing state back to op-reth.
                // We can live without this for now because Op syncs up the executed block using new_payload
                // calls, but eventually we would want to return the executed block here.
                None,
            ),
            fb_payload,
            bundle,
        ))
    }

    /// Spawns a task responsible for cancelling, and initiating building of new flashblock payloads.
    ///
    /// A Flashblock job will be cancelled under the following conditions:
    /// - If the parent `cancel` is dropped.
    /// - If the `flashblock_interval` has been exceeded.
    /// - If the `max_flashblocks` has been exceeded.
    fn spawn_flashblock_job_manager(&self, tx: tokio::sync::mpsc::Sender<()>) {
        let block_time = self.block_time;
        let flashblock_interval = self.flashblock_interval;
        let cancel = self.cancel.clone();
        let span = span!(tracing::Level::DEBUG, "flashblock_job_manager");
        tokio::spawn(async move {
            let _enter = span.enter();
            debug!(target: "payload_builder", "flashblock job manager started");

            let mut flashblock_interval =
                tokio::time::interval(tokio::time::Duration::from_millis(flashblock_interval));
            let mut block_interval =
                tokio::time::interval(tokio::time::Duration::from_millis(block_time));

            block_interval.tick().await;
            flashblock_interval.tick().await;

            tokio::select! {
                _ = block_interval.tick() => {
                    debug!(target: "payload_builder", "block interval exceeded, cancelling current job");
                    // block interval exceeded, cancel the current job
                    // and drop the sender to resolve the most recent payload.
                    drop(tx);
                },

                _ = async {
                    loop {
                        let instant = Instant::now();
                        let _tx = tx.clone();
                        if cancel.is_cancelled() {
                            debug!(target: "payload_builder", "parent cancel was dropped, cancelling flashblock job");
                            // parent cancel was dropped by the payload jobs generator
                            // cancel the child job, and drop the sender
                            break;
                        }

                        tokio::select! {
                            _ = flashblock_interval.tick() => {
                                let elapsed = instant.elapsed();
                                debug!(target: "payload_builder", ?elapsed, "flashblock interval exceeded, cancelling current job");
                                // queue the next flashblock job, but there's no benefit in cancelling the current job
                                // here we are exceeding `flashblock_interval` on the current job, but we want to give it `block_time` to finish.
                                // because the next job will also exceed `flashblock_interval`.
                                if let Err(err) = tx.send(()).await {
                                    warn!(target: "payload_builder", %err, "failed to send flashblock job notification");
                                }
                            },
                       }
                    }
                } => {
                    // either the parent payload job was cancelled, or all flashblocks were built.
                    // in either case, drop the sender to resolve the most recent payload.
                }
            }
        });
    }
}

#[derive(Default, Debug)]
pub struct ExecutionInfo<Extra: Debug + Default = ()> {
    /// All executed transactions (unrecovered).
    pub executed_transactions: Vec<OpTransactionSigned>,
    /// The recovered senders for the executed transactions.
    pub executed_senders: Vec<Address>,
    /// The transaction receipts
    pub receipts: Vec<OpReceipt>,
    /// All gas used so far
    pub cumulative_gas_used: u64,
    /// Estimated DA size
    pub cumulative_da_bytes_used: u64,
    /// Tracks fees from executed mempool transactions
    pub total_fees: U256,
    /// Extra execution information that can be attached by individual builders.
    pub extra: Extra,
}

impl<T: Debug + Default> ExecutionInfo<T> {
    /// Create a new instance with allocated slots.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            executed_transactions: Vec::with_capacity(capacity),
            executed_senders: Vec::with_capacity(capacity),
            receipts: Vec::with_capacity(capacity),
            cumulative_gas_used: 0,
            cumulative_da_bytes_used: 0,
            total_fees: U256::ZERO,
            extra: Default::default(),
        }
    }

    pub fn is_tx_over_limits(
        &self,
        tx_da_size: u64,
        block_gas_limit: u64,
        tx_data_limit: Option<u64>,
        block_data_limit: Option<u64>,
        tx_gas_limit: u64,
    ) -> bool {
        if tx_data_limit.is_some_and(|da_limit| tx_da_size > da_limit) {
            return true;
        }

        if block_data_limit
            .is_some_and(|da_limit| self.cumulative_da_bytes_used + tx_da_size > da_limit)
        {
            return true;
        }

        self.cumulative_gas_used + tx_gas_limit > block_gas_limit
    }
}

#[derive(Debug, Default)]
pub struct FlashblocksExecutionMetadata {
    pub last_flashblock_offset: usize,
}
