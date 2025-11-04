use alloy_eips::Decodable2718;
use alloy_op_evm::OpBlockExecutionCtx;
use alloy_primitives::Address;
use eyre::eyre::OptionExt as _;
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use flashblocks_primitives::{p2p::AuthorizedPayload, primitives::FlashblocksPayloadV1};
use futures::StreamExt as _;
use op_alloy_consensus::OpTxEnvelope;
use parking_lot::RwLock;
use reth_chain_state::{ExecutedBlockWithTrieUpdates, ExecutedTrieUpdates};
use reth_evm::execute::{BlockAssembler, BlockAssemblerInput};
use reth_node_api::{BuiltPayload as _, Events, FullNodeTypes, NodeTypes};
use reth_node_builder::BuilderContext;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::OpNextBlockEnvAttributes;
use reth_optimism_node::{OpBuiltPayload, OpEngineTypes, OpEvmConfig};
use reth_optimism_primitives::OpPrimitives;
use reth_primitives::{transaction::SignedTransaction, RecoveredBlock};
use reth_provider::{ExecutionOutcome, HeaderProvider, StateProviderFactory};
use revm::database::BundleAccount;
use std::{collections::HashMap, sync::Arc};
use tokio::{sync::broadcast, time::Instant};
use tracing::{error, info, trace};

use crate::{
    assembler::FlashblocksBlockAssembler,
    executor::{
        bal_executor::{compute_state_root, execute_transactions},
        factory::FlashblocksBlockExecutorFactory,
    },
};
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
    pending_block: tokio::sync::watch::Sender<Option<ExecutedBlockWithTrieUpdates<OpPrimitives>>>,
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
        pending_block: tokio::sync::watch::Sender<
            Option<ExecutedBlockWithTrieUpdates<OpPrimitives>>,
        >,
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
                        &chain_spec,
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
    ) -> tokio::sync::watch::Receiver<Option<ExecutedBlockWithTrieUpdates<OpPrimitives>>> {
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
    state_executor: &FlashblocksExecutionCoordinator,
    chain_spec: &OpChainSpec,
    flashblock: FlashblocksPayloadV1,
    pending_block: tokio::sync::watch::Sender<Option<ExecutedBlockWithTrieUpdates<OpPrimitives>>>,
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
    } = *state_executor.inner.write();

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

    let transactions = flashblock
        .diff()
        .transactions
        .iter()
        .map(|b| {
            let tx: OpTxEnvelope = Decodable2718::decode_2718_exact(b)?;
            Ok(tx.try_into_recovered().unwrap())
        })
        .collect::<eyre::Result<Vec<_>>>()?;

    let senders = transactions
        .clone()
        .into_iter()
        .map(|tx| tx.into_parts().1)
        .collect::<Vec<_>>();

    let sealed_header = provider
        .sealed_header_by_hash(base.parent_hash)?
        .ok_or_eyre(format!("missing sealed header: {}", base.parent_hash))?;

    // We can now create the block executor, and transform the BAL into the expected hashed post state.
    //
    // Transaction Execution, and State Transition can be performed in parellel.
    // We need to ensure that transaction execution produces the same BAL as provided in the flashblock.

    let provider_clone = provider.clone();
    let evm_config = evm_config.clone();
    let base = base.clone();

    let latest_bundle = latest_payload.as_ref().map(|(p, _)| {
        p.executed_block()
            .unwrap() // Safe unwrap
            .block
            .execution_output
            .bundle
            .clone()
    });

    let state_provider = Arc::new(provider_clone.state_by_block_hash(sealed_header.hash())?);

    let sealed_header = Arc::new(
        provider_clone
            .sealed_header_by_hash(base.clone().parent_hash)?
            .ok_or_eyre(format!(
                "missing sealed header: {}",
                base.clone().parent_hash
            ))?,
    );

    let attributes = OpNextBlockEnvAttributes {
        timestamp: base.timestamp,
        suggested_fee_recipient: base.fee_recipient,
        prev_randao: base.prev_randao,
        gas_limit: base.gas_limit,
        parent_beacon_block_root: Some(base.parent_beacon_block_root),
        extra_data: base.extra_data.clone(),
    };

    // Prepare EVM.
    let execution_context = OpBlockExecutionCtx {
        parent_hash: base.parent_hash,
        parent_beacon_block_root: Some(base.parent_beacon_block_root),
        extra_data: base.extra_data.clone(),
    };

    let execution_context_clone = execution_context.clone();

    let state_provider_clone = state_provider.clone();
    let state_provider_clone2 = state_provider.clone();

    let sealed_header_clone = sealed_header.clone();
    let transactions_clone = transactions.clone();

    let before_execution = Instant::now();

    let latest_payload_clone = latest_payload.clone();
    let (execution_result, state_root_result) = if let Some(access_list_data) =
        flashblock.diff().access_list_data.as_ref()
    {
        info!(target: "flashblocks::state_executor", "executing flashblock with access list");
        let (execution_result, state_root_result) = rayon::join(
            move || {
                execute_transactions(
                    transactions_clone.clone(),
                    &evm_config,
                    sealed_header.clone(),
                    state_provider_clone.clone(),
                    &attributes,
                    latest_bundle,
                    execution_context_clone.clone(),
                    chain_spec,
                    latest_payload_clone.as_ref().map(|(p, _)| p.clone()),
                    Some(&access_list_data.access_list.clone()),
                )
            },
            move || {
                let optimistic_bundle: HashMap<Address, BundleAccount> =
                    access_list_data.access_list.clone().into();

                compute_state_root(state_provider_clone2.clone(), &optimistic_bundle)
            },
        );

        (execution_result?, state_root_result?)
    } else {
        info!(target: "flashblocks::state_executor", "executing flashblock without access list");
        let execution_result = execute_transactions(
            transactions_clone.clone(),
            &evm_config,
            sealed_header.clone(),
            state_provider_clone.clone(),
            &attributes,
            latest_bundle,
            execution_context_clone.clone(),
            chain_spec,
            latest_payload_clone.as_ref().map(|(p, _)| p.clone()),
            None,
        )?;

        let converted: HashMap<Address, BundleAccount> =
            execution_result.clone().0.state.into_iter().collect();

        let state_root_result = compute_state_root(state_provider_clone2.clone(), &converted)?;

        (execution_result, state_root_result)
    };

    let elapsed = before_execution.elapsed().as_millis();
    tracing::info!(target: "flashblocks::state_executor", "time taken to get here: {:?}, bal enabled: {}", elapsed, flashblock.diff().access_list_data.is_some());

    let (bundle_state, block_execution_result, _access_list, evm_env, total_fees) =
        execution_result;

    let (state_root, trie_updates, hashed_state) = state_root_result;

    assert_eq!(
        state_root,
        flashblock.diff().state_root,
        "Computed state root does not match state root provided in flashblock"
    );

    let block_assembler = FlashblocksBlockAssembler::new(Arc::new(chain_spec.clone()));

    let block = block_assembler.assemble_block(BlockAssemblerInput::<
        '_,
        '_,
        FlashblocksBlockExecutorFactory,
    >::new(
        evm_env,
        execution_context.clone(),
        &sealed_header_clone.clone(),
        transactions
            .into_iter()
            .map(|tx| tx.into_parts().0)
            .collect(),
        &block_execution_result,
        &bundle_state,
        &state_provider,
        state_root,
    ))?;

    let block = RecoveredBlock::new_unhashed(block, senders);
    let sealed_block = Arc::new(block.sealed_block().clone());

    let recovered_block = Arc::new(block);

    trace!(target: "flashblocks::state_executor", hash = %recovered_block.hash(), "setting latest payload");

    let execution_outcome = ExecutionOutcome::new(
        bundle_state.clone(),
        vec![block_execution_result.receipts],
        0,
        vec![block_execution_result.requests],
    );

    let executed_block = ExecutedBlockWithTrieUpdates::new(
        recovered_block,
        Arc::new(execution_outcome),
        Arc::new(hashed_state),
        ExecutedTrieUpdates::Present(Arc::new(trie_updates)),
    );

    let payload = OpBuiltPayload::new(
        *flashblock.payload_id(),
        sealed_block,
        latest_payload
            .as_ref()
            .map(|p| p.0.fees() + total_fees)
            .unwrap_or(total_fees),
        Some(executed_block),
    );

    flashblocks.push(flashblock)?;

    // construct the full payload
    *latest_payload = Some((payload.clone(), index));

    pending_block.send_replace(payload.executed_block());

    state_executor.broadcast_payload(Events::BuiltPayload(payload), payload_events.clone())?;

    Ok(())
}
