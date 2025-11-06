use alloy_evm::revm::database::State;
use alloy_op_evm::OpBlockExecutionCtx;
use eyre::eyre::OptionExt as _;
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use flashblocks_primitives::{p2p::AuthorizedPayload, primitives::FlashblocksPayloadV1};
use futures::StreamExt as _;
use parking_lot::RwLock;
use reth::revm::database::StateProviderDatabase;
use reth_chain_state::ExecutedBlock;
use reth_evm::{ConfigureEvm, EvmFactory};
use reth_node_api::{BuiltPayload as _, Events, FullNodeTypes, NodeTypes};
use reth_node_builder::BuilderContext;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::OpRethReceiptBuilder;
use reth_optimism_node::{OpBuiltPayload, OpEngineTypes, OpEvmConfig};
use reth_optimism_primitives::OpPrimitives;

use reth_provider::{HeaderProvider, StateProvider, StateProviderFactory};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{error, trace};

use crate::executor::bal_executor::BalBlockExecutor;
use flashblocks_primitives::flashblocks::{Flashblock, Flashblocks};

/// The current state of all known pre confirmations received over the P2P layer
/// or generated from the payload building job of this node.
///
/// The state is flushed when FCU is received with a parent hash that matches the block hash
/// of the latest pre confirmation _or_ when an FCU is received that does not match the latest pre confirmation,
/// in which case the pre confirmations were not included as part of the canonical chain.
#[derive(Debug, Clone)]
pub struct FlashblocksExecutionCoordinator {
    inner: Arc<RwLock<FlashblocksExecutionCoordinatorInner>>,
    p2p_handle: FlashblocksHandle,
    pending_block: tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
}

#[derive(Debug, Clone)]
pub struct FlashblocksExecutionCoordinatorInner {
    /// List of flashblocks for the current payload
    flashblocks: Flashblocks,
    /// The latest built payload with its associated flashblock index
    latest_payload: Option<(OpBuiltPayload, u64)>,
    /// Broadcast channel for built payload events
    payload_events: Option<broadcast::Sender<Events<OpEngineTypes>>>,
}

impl FlashblocksExecutionCoordinator {
    /// Creates a new instance of [`FlashblocksStateExecutor`].
    ///
    /// This function spawn a task that handles updates. It should only be called once.
    pub fn new(
        p2p_handle: FlashblocksHandle,
        pending_block: tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
    ) -> Self {
        let inner = Arc::new(RwLock::new(FlashblocksExecutionCoordinatorInner {
            flashblocks: Default::default(),
            latest_payload: None,
            payload_events: None,
        }));

        Self {
            inner,
            p2p_handle,
            pending_block,
        }
    }

    /// Launches the executor to listen for new flashblocks and build payloads.
    pub fn launch<Node>(&self, ctx: &BuilderContext<Node>, evm_config: OpEvmConfig)
    where
        Node: FullNodeTypes,
        Node::Provider: StateProviderFactory + HeaderProvider<Header = alloy_consensus::Header>,
        Node::Types: NodeTypes<ChainSpec = OpChainSpec>,
    {
        let mut stream = self.p2p_handle.flashblock_stream();
        let this = self.clone();
        let provider = ctx.provider().clone();
        let chain_spec = ctx.chain_spec().clone();

        let pending_block = self.pending_block.clone();

        ctx.task_executor()
            .spawn_critical("flashblocks executor", async move {
                while let Some(flashblock) = stream.next().await {
                    let provider = provider.clone();
                    if let Err(e) = process_flashblock(
                        provider,
                        &evm_config,
                        &this,
                        chain_spec.clone(),
                        flashblock,
                        pending_block.clone(),
                    ) {
                        error!("error processing flashblock: {e:?}")
                    }
                }
            });
    }

    pub fn publish_built_payload(
        &self,
        authorized_payload: AuthorizedPayload<FlashblocksPayloadV1>,
        built_payload: OpBuiltPayload,
    ) -> eyre::Result<()> {
        let flashblock = authorized_payload.msg().clone();

        let FlashblocksExecutionCoordinatorInner {
            ref mut flashblocks,
            ref mut latest_payload,
            ..
        } = *self.inner.write();

        let index = flashblock.index;
        let flashblock = Flashblock { flashblock };
        flashblocks.push(flashblock.clone())?;

        *latest_payload = Some((built_payload, index));

        self.p2p_handle.publish_new(authorized_payload.clone())?;

        Ok(())
    }

    /// Returns a reference to the latest flashblock.
    pub fn last(&self) -> Flashblock {
        self.inner.read().flashblocks.last().clone()
    }

    /// Returns a reference to the latest flashblock.
    pub fn flashblocks(&self) -> Flashblocks {
        self.inner.read().flashblocks.clone()
    }

    /// Returns a receiver for the pending block.
    pub fn pending_block(
        &self,
    ) -> tokio::sync::watch::Receiver<Option<ExecutedBlock<OpPrimitives>>> {
        self.pending_block.subscribe()
    }

    /// Registers a new broadcast channel for built payloads.
    pub fn register_payload_events(&self, tx: broadcast::Sender<Events<OpEngineTypes>>) {
        self.inner.write().payload_events = Some(tx);
    }

    /// Broadcasts a new payload to cache in the in memory tree.
    pub fn broadcast_payload(
        &self,
        event: Events<OpEngineTypes>,
        payload_events: Option<broadcast::Sender<Events<OpEngineTypes>>>,
    ) -> eyre::Result<()> {
        if let Some(payload_events) = payload_events {
            if let Err(e) = payload_events.send(event) {
                error!("error broadcasting payload: {e:?}");
            }
        }
        Ok(())
    }
}

fn process_flashblock<Provider>(
    provider: Provider,
    evm_config: &OpEvmConfig,
    coordinator: &FlashblocksExecutionCoordinator,
    chain_spec: Arc<OpChainSpec>,
    flashblock: FlashblocksPayloadV1,
    pending_block: tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
) -> eyre::Result<()>
where
    Provider:
        StateProviderFactory + HeaderProvider<Header = alloy_consensus::Header> + Clone + 'static,
{
    trace!(
        target: "flashblocks::state_executor",
        id = %flashblock.payload_id,
        index = %flashblock.index,
        min_tx_index = %flashblock.diff.access_list_data.as_ref().map_or(0, |d| d.access_list.min_tx_index),
        max_tx_index = %flashblock.diff.access_list_data.as_ref().map_or(0, |d| d.access_list.max_tx_index),
        "processing flashblock"
    );

    let FlashblocksExecutionCoordinatorInner {
        ref mut flashblocks,
        ref mut latest_payload,
        ref mut payload_events,
    } = *coordinator.inner.write();

    let flashblock = Flashblock { flashblock };

    if let Some(latest_payload) = latest_payload {
        if latest_payload.0.id() == flashblock.flashblock.payload_id
            && latest_payload.1 >= flashblock.flashblock.index
        {
            // Already processed this flashblock. This happens when set directly
            // from publish_build_payload. Since we already built the payload, no need
            // to do it again.
            pending_block.send_replace(latest_payload.0.executed_block());
            return Ok(());
        }
    }

    // If for whatever reason we are not processing flashblocks in order
    // we will error and return here.
    let base = if flashblocks.is_new_payload(&flashblock)? {
        *latest_payload = None;
        // safe unwrap from check in is_new_payload
        flashblock.base().unwrap()
    } else {
        flashblocks.base()
    };

    let index = flashblock.flashblock().index;

    let sealed_header = provider
        .sealed_header_by_hash(base.parent_hash)?
        .ok_or_eyre(format!("missing sealed header: {}", base.parent_hash))?;

    let state_provider: Arc<dyn StateProvider> =
        provider.state_by_block_hash(sealed_header.hash())?.into();

    let sealed_header = Arc::new(
        provider
            .sealed_header_by_hash(base.clone().parent_hash)?
            .ok_or_eyre(format!(
                "missing sealed header: {}",
                base.clone().parent_hash
            ))?,
    );

    // let attributes = OpPayloadBuilderAttributes {
    //     payload_attributes: eth_attrs,
    //     no_tx_pool: true,
    //     transactions: transactions.clone(),
    //     gas_limit: None,
    //     eip_1559_params: Some(eip1559[1..=8].try_into()?),
    //     min_base_fee: None,
    // };
    //
    // let sealed_header = provider
    //     .sealed_header_by_hash(base.parent_hash)?
    //     .ok_or_eyre(format!("missing sealed header: {}", base.parent_hash))?;
    //
    // let state_provider = provider.state_by_block_hash(base.parent_hash)?;
    //
    // let config = PayloadConfig::new(Arc::new(sealed_header), attributes);
    // let builder_ctx = payload_builder_ctx_builder.build(
    //     provider.clone(),
    //     evm_config.clone(),
    //     state_executor.builder_config.clone(),
    //     config,
    //     &cancel,
    //     latest_payload.as_ref().map(|p| p.0.clone()),
    // );
    //
    // let best = |_| BestPayloadTransactions::new(vec![].into_iter());
    // let db = StateProviderDatabase::new(&state_provider);
    //
    // let outcome = FlashblockBuilder::new(best).build(
    //     pool.clone(),
    //     db,
    //     &state_provider,
    //     &builder_ctx,
    //     latest_payload.as_ref().map(|p| p.0.clone()),
    // )?;

    let execution_context = OpBlockExecutionCtx {
        parent_hash: base.parent_hash,
        parent_beacon_block_root: Some(base.parent_beacon_block_root),
        extra_data: base.extra_data.clone(),
    };

    let payload = if flashblock.diff().access_list_data.is_some() {
        let block_validator = BalBlockExecutor::<OpRethReceiptBuilder, OpChainSpec>::new(
            chain_spec.clone(),
            OpRethReceiptBuilder::default(),
            execution_context,
            evm_config.clone(),
        );
        block_validator.validate_and_execute_diff_parallel(
            state_provider,
            latest_payload.as_ref().map(|(p, _)| p.clone()),
            flashblock.diff().clone(),
            &sealed_header,
            chain_spec,
            *flashblock.payload_id(),
        )?
    } else {
        let db = StateProviderDatabase::new(state_provider);
        let mut state = State::builder().with_database(db).build();
        let env = evm_config.evm_env(sealed_header.header())?;
        let evm = evm_config
            .executor_factory
            .evm_factory()
            .create_evm(&mut state, env);
        let executor = evm_config.create_executor(evm, execution_context);
        executor.execute
    };

    flashblocks.push(flashblock)?;

    // construct the full payload
    *latest_payload = Some((payload.clone(), index));

    pending_block.send_replace(payload.executed_block());

    coordinator.broadcast_payload(Events::BuiltPayload(payload), payload_events.clone())?;

    Ok(())
}
