use std::sync::{Arc, Mutex};

use futures_util::{sink::SinkExt, FutureExt};
use reth::{
    api::{BuiltPayload, PayloadBuilderAttributes, PayloadBuilderError},
    chainspec::EthChainSpec,
    revm::{database::StateProviderDatabase, Database, State},
};
use reth_basic_payload_builder::{BuildArguments, BuildOutcome, BuildOutcomeKind};
use reth_basic_payload_builder::{MissingPayloadBehaviour, PayloadBuilder, PayloadConfig};
use reth_evm::{execute::BlockBuilder, ConfigureEvm, Evm};
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
use reth_payload_util::NoopPayloadTransactions;
use reth_primitives::{NodePrimitives, TxTy};
use reth_provider::{ChainSpecProvider, ProviderError, StateProvider, StateProviderFactory};
use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction, TransactionPool};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tokio_tungstenite::{accept_async, WebSocketStream};
use tracing::warn;

pub trait FlashblockBuilder: BlockBuilder {
    fn build_flashblock<'a, Txs>(
        db: impl Database<Error = ProviderError>,
        state_provider: impl StateProvider,
        best: impl FnOnce(BestTransactionsAttributes) -> Txs + Send + Sync + 'a,
    );
}

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
        let BuildArguments {
            mut cached_reads,
            config,
            cancel,
            best_payload,
        } = args;

        let parent_hash = config.parent_header.hash();

        // TODO: create generic payload builder ctx,
        let ctx = OpPayloadBuilderCtx {
            evm_config: self.evm_config.clone(),
            da_config: self.config.da_config.clone(),
            chain_spec: self.client.chain_spec(),
            config,
            cancel,
            best_payload,
        };

        let state_provider = self.client.state_by_block_hash(parent_hash)?;
        let state: StateProviderDatabase<&Box<dyn StateProvider>> =
            StateProviderDatabase::new(&state_provider);

        let mut db = State::builder()
            .with_database(state)
            .with_bundle_update()
            .build();

        let mut builder = ctx.block_builder(&mut db)?;

        // apply pre-execution changes
        builder.apply_pre_execution_changes().map_err(|err| {
            warn!(target: "payload_builder", %err, "failed to apply pre-execution changes");
            PayloadBuilderError::Internal(err.into())
        })?;

        // execute sequencer transactions
        let mut info = ctx.execute_sequencer_transactions(&mut builder)?;

        if !ctx.attributes().no_tx_pool {
            let tx_attrs = ctx.best_transaction_attributes(builder.evm_mut().block());

            // TODO: dynamically update amount of flashblocks
            let num_flashblocks = 4;
            for _ in 0..num_flashblocks {
                // TODO: update to ensure the pool is being updated
                let pool = self.pool.clone();
                let best_txs = self.best_transactions.best_transactions(pool, tx_attrs);

                if ctx
                    .execute_best_transactions(&mut info, &mut builder, best_txs)?
                    .is_some()
                {
                    return Ok(BuildOutcome::Cancelled);
                }

                let (payload, mut fb_payload, new_bundle_state) =
                    build_flashblock(db, &ctx, &mut info)?;

                // TODO: update the stream in a separate PR
                let _ = self.send_message(serde_json::to_string(&fb_payload).unwrap_or_default());
            }
        }

        todo!("Return the built block")
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

pub fn build_flashblock<EvmConfig, ChainSpec, DB, P>(
    mut state: State<DB>,
    ctx: &OpPayloadBuilderCtx<EvmConfig, ChainSpec>,
    info: &mut ExecutionInfo,
) -> Result<(OpBuiltPayload, FlashblocksPayloadV1, BundleState), PayloadBuilderError>
// where
//     EvmConfig: ConfigureEvmFor<N>,
//     ChainSpec: EthChainSpec + OpHardforks,
//     N: OpPayloadPrimitives<_TX = OpTransactionSigned>,
//     DB: Database<Error = ProviderError> + AsRef<P>,
//     P: StateRootProvider + HashedPostStateProvider + StorageRootProvider,
{
    // TODO:
    todo!()
}
