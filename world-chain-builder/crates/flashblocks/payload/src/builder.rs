use alloy_consensus::{Header, EMPTY_OMMER_ROOT_HASH};
use alloy_eips::merge::BEACON_NONCE;
use alloy_primitives::{Address, B256, U256};
use futures_util::{sink::SinkExt, FutureExt};
use reth::{
    api::{BuiltPayload, PayloadBuilderAttributes, PayloadBuilderError},
    chainspec::EthChainSpec,
    revm::{database::StateProviderDatabase, db::BundleState, Database, State},
};
use reth_basic_payload_builder::{BuildArguments, BuildOutcome, BuildOutcomeKind};
use reth_basic_payload_builder::{MissingPayloadBehaviour, PayloadBuilder, PayloadConfig};
use reth_chain_state::{ExecutedBlock, ExecutedBlockWithTrieUpdates};
use reth_evm::block::BlockExecutorFactory;
use reth_evm::{
    execute::{BlockBuilder, BlockBuilderOutcome},
    ConfigureEvm, Evm,
};
use reth_optimism_consensus::calculate_receipt_root_no_memo_optimism;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::OpNextBlockEnvAttributes;
use reth_optimism_payload_builder::{
    builder::OpPayloadTransactions,
    payload::{OpBuiltPayload, OpPayloadBuilderAttributes},
};
use reth_optimism_payload_builder::{
    builder::{ExecutionInfo, OpPayloadBuilderCtx},
    config::OpBuilderConfig,
    OpPayloadPrimitives,
};
use reth_optimism_primitives::OpTransactionSigned;
use reth_payload_util::{NoopPayloadTransactions, PayloadTransactions};
use reth_primitives::{BlockBody, NodePrimitives, TxTy};
use reth_primitives_traits::proofs;
use reth_provider::{
    ChainSpecProvider, ExecutionOutcome, HashedPostStateProvider, ProviderError, StateProvider,
    StateProviderFactory, StateRootProvider, StorageRootProvider,
};
use reth_transaction_pool::{
    BestTransactionsAttributes, EthPoolTransaction, PoolTransaction, TransactionPool,
};
use revm::database::states::bundle_state::BundleRetention;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tokio_tungstenite::{accept_async, WebSocketStream};
use tracing::{debug, info, warn};

use crate::payload::{
    ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1, FlashblocksMetadata,
    FlashblocksPayloadV1,
};

/// Optimism's payload builder
#[derive(Debug, Clone)]
pub struct FlashBlocksPayloadBuilder<Pool, Client, Evm, Txs = ()> {
    /// The rollup's compute pending block configuration option.
    // TODO(clabby): Implement this feature.
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
    /// Channel sender for publishing messages
    pub tx: mpsc::UnboundedSender<String>,
}

impl<Pool, Client, Evm, Txs> FlashBlocksPayloadBuilder<Pool, Client, Evm, Txs> {
    pub fn new() -> Self {
        todo!()
    }

    /// Start the WebSocket server
    pub async fn start_ws(subscribers: Arc<Mutex<Vec<WebSocketStream<TcpStream>>>>, addr: &str) {
        let listener = TcpListener::bind(addr).await.unwrap();
        let subscribers = subscribers.clone();

        tracing::info!("Starting WebSocket server on {}", addr);

        while let Ok((stream, _)) = listener.accept().await {
            tracing::info!("Accepted websocket connection");
            let subscribers = subscribers.clone();

            tokio::spawn(async move {
                match accept_async(stream).await {
                    Ok(ws_stream) => {
                        let mut subs = subscribers.lock().unwrap();
                        subs.push(ws_stream);
                    }
                    Err(e) => eprintln!("Error accepting websocket connection: {}", e),
                }
            });
        }
    }

    /// Background task that handles publishing messages to WebSocket subscribers
    fn publish_task(
        mut rx: mpsc::UnboundedReceiver<String>,
        subscribers: Arc<Mutex<Vec<WebSocketStream<TcpStream>>>>,
    ) {
        tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                let mut subscribers = subscribers.lock().unwrap();

                // Remove disconnected subscribers and send message to connected ones
                subscribers.retain_mut(|ws_stream| {
                    let message = message.clone();
                    async move {
                        ws_stream
                            .send(tokio_tungstenite::tungstenite::Message::Text(
                                message.into(),
                            ))
                            .await
                            .is_ok()
                    }
                    .now_or_never()
                    .unwrap_or(false)
                });
            }
        });
    }

    /// Send a message to be published
    pub fn send_message(&self, message: String) -> Result<(), Box<dyn std::error::Error>> {
        self.tx.send(message).map_err(|e| e.into())
    }
}

impl<Pool, Client, Evm, N, T> FlashBlocksPayloadBuilder<Pool, Client, Evm, T>
where
    Pool: TransactionPool<Transaction: EthPoolTransaction<Consensus = N::SignedTx>>,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec: EthChainSpec + OpHardforks>,
    N: OpPayloadPrimitives,
    Evm: ConfigureEvm<Primitives = N, NextBlockEnvCtx = OpNextBlockEnvAttributes>,
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
        best: impl Fn(BestTransactionsAttributes) -> Txs + Send + Sync + 'a,
    ) -> Result<BuildOutcome<OpBuiltPayload<N>>, PayloadBuilderError>
    where
        Txs: PayloadTransactions<Transaction: PoolTransaction<Consensus = N::SignedTx>>,
    {
        let BuildArguments {
            mut cached_reads,
            config,
            cancel,
            best_payload,
        } = args;

        let ctx = OpPayloadBuilderCtx {
            evm_config: self.evm_config.clone(),
            da_config: self.config.da_config.clone(),
            chain_spec: self.client.chain_spec(),
            config,
            cancel,
            best_payload,
        };

        let builder = FlashblockBuilder::new(best, self.tx.clone());
        let state_provider = self.client.state_by_block_hash(ctx.parent().hash())?;
        let state = StateProviderDatabase::new(&state_provider);

        if ctx.attributes().no_tx_pool {
            builder.build(state, &state_provider, ctx)
        } else {
            // sequencer mode we can reuse cachedreads from previous runs
            builder.build(cached_reads.as_db_mut(state), &state_provider, ctx)
        }
        .map(|out| out.with_cached_reads(cached_reads))
    }
}

impl<Pool, Client, Evm, N, Txs> PayloadBuilder for FlashBlocksPayloadBuilder<Pool, Client, Evm, Txs>
where
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec: EthChainSpec + OpHardforks> + Clone,
    N: OpPayloadPrimitives,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = N::SignedTx>>,
    Evm: ConfigureEvm<Primitives = N, NextBlockEnvCtx = OpNextBlockEnvAttributes>,
    Txs: OpPayloadTransactions<Pool::Transaction>,
{
    type Attributes = OpPayloadBuilderAttributes<N::SignedTx>;
    type BuiltPayload = OpBuiltPayload<N>;

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
        config: PayloadConfig<Self::Attributes>,
    ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        todo!()
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
pub struct FlashblockBuilder<'a, Txs> {
    /// Yields the best transaction to include if transactions from the mempool are allowed.
    best: Box<dyn Fn(BestTransactionsAttributes) -> Txs + 'a>,
    /// Channel sender for publishing messages
    pub tx: mpsc::UnboundedSender<String>,
}

impl<'a, Txs> FlashblockBuilder<'a, Txs> {
    /// Creates a new [`OpBuilder`].
    pub fn new(
        best: impl Fn(BestTransactionsAttributes) -> Txs + Send + Sync + 'a,
        tx: mpsc::UnboundedSender<String>,
    ) -> Self {
        Self {
            best: Box::new(best),
            tx,
        }
    }
}

impl<Txs> FlashblockBuilder<'_, Txs> {
    /// Builds the payload on top of the state.
    pub fn build<EvmConfig, ChainSpec, N>(
        self,
        db: impl Database<Error = ProviderError>,
        state_provider: impl StateProvider,
        // TODO: make ctx generic
        ctx: OpPayloadBuilderCtx<EvmConfig, ChainSpec>,
    ) -> Result<BuildOutcomeKind<OpBuiltPayload<N>>, PayloadBuilderError>
    where
        EvmConfig: ConfigureEvm<Primitives = N, NextBlockEnvCtx = OpNextBlockEnvAttributes>,
        ChainSpec: EthChainSpec + OpHardforks,
        N: OpPayloadPrimitives,
        Txs: PayloadTransactions<Transaction: PoolTransaction<Consensus = N::SignedTx>>,
    {
        let Self { best, tx } = self;
        debug!(target: "payload_builder", id=%ctx.payload_id(), parent_header = ?ctx.parent().hash(), parent_number = ctx.parent().number, "building new payload");

        // NOTE: we do not need to init this every time
        let mut db = State::builder()
            .with_database(db)
            .with_bundle_update()
            .build();

        let mut builder = ctx.block_builder(&mut db)?;

        // 1. apply pre-execution changes
        builder.apply_pre_execution_changes().map_err(|err| {
            warn!(target: "payload_builder", %err, "failed to apply pre-execution changes");
            PayloadBuilderError::Internal(err.into())
        })?;

        // 2. execute sequencer transactions
        let mut info = ctx.execute_sequencer_transactions(&mut builder)?;

        if !ctx.attributes().no_tx_pool {
            // let flashblock_gas_limit =
            //     ctx.block_gas_limit() / (self.chain_block_time / self.flashblock_block_time);

            // TODO: make this dynamic
            let num_flashblocks = 4;

            for _ in 0..num_flashblocks {
                let best_txs = best(ctx.best_transaction_attributes(builder.evm_mut().block()));
                if ctx
                    .execute_best_transactions(&mut info, &mut builder, best_txs)?
                    .is_some()
                {
                    return Ok(BuildOutcomeKind::Cancelled);
                }
            }

            // TODO: this should be builder.finish_flashblock(), updates the bundle state, etc.
            let (_, fb_payload, mut bundle_state) =
                build_flashblock(&mut builder, &ctx, &mut info)?;

            // TODO: need to update db with bundle state
            tx.send(serde_json::to_string(&fb_payload).unwrap_or_default())
                .expect("TODO: handle error");
        }

        // check if the new payload is even more valuable
        if !ctx.is_better_payload(info.total_fees) {
            // can skip building the block
            return Ok(BuildOutcomeKind::Aborted {
                fees: info.total_fees,
            });
        }

        let BlockBuilderOutcome {
            execution_result,
            hashed_state,
            trie_updates,
            block,
        } = builder.finish(state_provider)?;

        let sealed_block = Arc::new(block.sealed_block().clone());
        debug!(target: "payload_builder", id=%ctx.attributes().payload_id(), sealed_block_header = ?sealed_block.header(), "sealed built block");

        let execution_outcome = ExecutionOutcome::new(
            db.take_bundle(),
            vec![execution_result.receipts],
            block.number,
            Vec::new(),
        );

        // create the executed block data
        let executed: ExecutedBlockWithTrieUpdates<N> = ExecutedBlockWithTrieUpdates {
            block: ExecutedBlock {
                recovered_block: Arc::new(block),
                execution_output: Arc::new(execution_outcome),
                hashed_state: Arc::new(hashed_state),
            },
            trie: Arc::new(trie_updates),
        };

        let no_tx_pool = ctx.attributes().no_tx_pool;

        let payload = OpBuiltPayload::new(
            ctx.payload_id(),
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
}

// /* TODO: currently this builds a flashblock and returns a built block to propose to the network.
// It looks like we only need to do some of thes calculations once when building the final block. This
// function should be updated and return all the necessary components needed to seal the block.
// */
pub fn build_flashblock<Evm, ChainSpec>(
    builder: &mut impl BlockBuilder,
    // builder: &mut impl BlockBuilder<Primitives = impl PoolTransaction>,
    ctx: &OpPayloadBuilderCtx<Evm, ChainSpec>,
    info: &mut ExecutionInfo,
) -> Result<(OpBuiltPayload, FlashblocksPayloadV1, BundleState), PayloadBuilderError>
where
    Evm: ConfigureEvm<Primitives: OpPayloadPrimitives, NextBlockEnvCtx = OpNextBlockEnvAttributes>,
    ChainSpec: EthChainSpec + OpHardforks,
    // DB: Database<Error = ProviderError> + AsRef<P>,
    // P: StateRootProvider + HashedPostStateProvider + StorageRootProvider,
{
    // // NOTE: ctx.withdrawals_root does not exist in latest reth
    // // let withdrawals_root = ctx.commit_withdrawals(&mut state)?;

    // // TODO: We must run this only once per block, but we are running it on every flashblock
    // // merge all transitions into bundle state, this would apply the withdrawal balance changes
    // // and 4788 contract call
    // state.merge_transitions(BundleRetention::Reverts);

    // let new_bundle = state.take_bundle();

    // // NOTE: ctx.block_number does not exist in latest reth
    // // let block_number = ctx.block_number();
    // // assert_eq!(block_number, ctx.parent().number + 1);

    // let block_number = ctx.parent().number + 1;
    // let execution_outcome = ExecutionOutcome::new(
    //     new_bundle.clone(),
    //     vec![info.receipts.clone()],
    //     block_number,
    //     vec![],
    // );
    // let receipts_root = execution_outcome
    //     .generic_receipts_root_slow(block_number, |receipts| {
    //         calculate_receipt_root_no_memo_optimism(
    //             receipts,
    //             &ctx.chain_spec,
    //             ctx.attributes().timestamp(),
    //         )
    //     })
    //     .expect("Number is in range");
    // let logs_bloom = execution_outcome
    //     .block_logs_bloom(block_number)
    //     .expect("Number is in range");

    // // // calculate the state root
    // let state_provider = state.database.as_ref();
    // let hashed_state = state_provider.hashed_post_state(execution_outcome.state());
    // let (state_root, _trie_output) = {
    //     state
    //         .database
    //         .as_ref()
    //         .state_root_with_updates(hashed_state.clone())
    //         .inspect_err(|err| {
    //             warn!(target: "payload_builder",
    //             parent_header=%ctx.parent().hash(),
    //                 %err,
    //                 "failed to calculate state root for payload"
    //             );
    //         })?
    // };

    // // create the block header
    // let transactions_root = proofs::calculate_transaction_root(&info.executed_transactions);

    // // OP doesn't support blobs/EIP-4844.
    // // https://specs.optimism.io/protocol/exec-engine.html#ecotone-disable-blob-transactions
    // // Need [Some] or [None] based on hardfork to match block hash.
    // let (excess_blob_gas, blob_gas_used) = ctx.blob_fields();
    // let extra_data = ctx.extra_data()?;

    // let header = Header {
    //     parent_hash: ctx.parent().hash(),
    //     ommers_hash: EMPTY_OMMER_ROOT_HASH,
    //     beneficiary: ctx.evm_env.block_env.coinbase,
    //     state_root,
    //     transactions_root,
    //     receipts_root,
    //     withdrawals_root,
    //     logs_bloom,
    //     timestamp: ctx.attributes().payload_attributes.timestamp,
    //     mix_hash: ctx.attributes().payload_attributes.prev_randao,
    //     nonce: BEACON_NONCE.into(),
    //     base_fee_per_gas: Some(ctx.base_fee()),
    //     number: ctx.parent().number + 1,
    //     gas_limit: ctx.block_gas_limit(),
    //     difficulty: U256::ZERO,
    //     gas_used: info.cumulative_gas_used,
    //     extra_data,
    //     parent_beacon_block_root: ctx.attributes().payload_attributes.parent_beacon_block_root,
    //     blob_gas_used,
    //     excess_blob_gas,
    //     requests_hash: None,
    // };

    // // seal the block
    // let block = N::Block::new(
    //     header,
    //     BlockBody {
    //         transactions: info.executed_transactions.clone(),
    //         ommers: vec![],
    //         withdrawals: ctx.withdrawals().cloned(),
    //     },
    // );

    // let sealed_block = Arc::new(block.seal_slow());
    // debug!(target: "payload_builder", ?sealed_block, "sealed built block");

    // let block_hash = sealed_block.hash();

    // // pick the new transactions from the info field and update the last flashblock index
    // let new_transactions = info.executed_transactions[info.last_flashblock_index..].to_vec();

    // let new_transactions_encoded = new_transactions
    //     .clone()
    //     .into_iter()
    //     .map(|tx| tx.encoded_2718().into())
    //     .collect::<Vec<_>>();

    // let new_receipts = info.receipts[info.last_flashblock_index..].to_vec();
    // info.last_flashblock_index = info.executed_transactions.len();
    // let receipts_with_hash = new_transactions
    //     .iter()
    //     .zip(new_receipts.iter())
    //     .map(|(tx, receipt)| (*tx.tx_hash(), receipt.clone()))
    //     .collect::<HashMap<B256, N::Receipt>>();
    // let new_account_balances = new_bundle
    //     .state
    //     .iter()
    //     .filter_map(|(address, account)| account.info.as_ref().map(|info| (*address, info.balance)))
    //     .collect::<HashMap<Address, U256>>();

    // let metadata = FlashblocksMetadata {
    //     receipts: receipts_with_hash,
    //     new_account_balances,
    //     block_number: ctx.parent().number + 1,
    // };

    // // Prepare the flashblocks message
    // let fb_payload = FlashblocksPayloadV1 {
    //     payload_id: ctx.payload_id(),
    //     index: 0, // TODO: fix this
    //     base: Some(ExecutionPayloadBaseV1 {
    //         parent_beacon_block_root: ctx
    //             .attributes()
    //             .payload_attributes
    //             .parent_beacon_block_root
    //             .unwrap(),
    //         parent_hash: ctx.parent().hash(),
    //         fee_recipient: ctx.attributes().suggested_fee_recipient(),
    //         prev_randao: ctx.attributes().payload_attributes.prev_randao,
    //         block_number: ctx.parent().number + 1,
    //         gas_limit: ctx.block_gas_limit(),
    //         timestamp: ctx.attributes().payload_attributes.timestamp,
    //         extra_data: ctx.extra_data()?,
    //         base_fee_per_gas: ctx.base_fee().try_into().unwrap(),
    //     }),
    //     diff: ExecutionPayloadFlashblockDeltaV1 {
    //         state_root,
    //         receipts_root,
    //         logs_bloom,
    //         gas_used: info.cumulative_gas_used,
    //         block_hash,
    //         transactions: new_transactions_encoded,
    //         withdrawals: ctx.withdrawals().cloned().unwrap_or_default().to_vec(),
    //     },
    //     metadata: serde_json::to_value(&metadata).unwrap_or_default(),
    // };

    // Ok((
    //     OpBuiltPayload::new(
    //         ctx.payload_id(),
    //         sealed_block,
    //         info.total_fees,
    //         // This must be set to NONE for now because we are doing merge transitions on every flashblock
    //         // when it should only happen once per block, thus, it returns a confusing state back to op-reth.
    //         // We can live without this for now because Op syncs up the executed block using new_payload
    //         // calls, but eventually we would want to return the executed block here.
    //         None,
    //     ),
    //     fb_payload,
    //     new_bundle,
    // ))

    todo!()
}
