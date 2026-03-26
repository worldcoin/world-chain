use alloy_primitives::U256;
use alloy_rpc_types_engine::{PayloadId, payload};
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use flashblocks_primitives::{
    p2p::{Authorized, AuthorizedPayload},
    primitives::FlashblocksPayloadV1,
};
use futures::{StreamExt, future, stream};
use metrics::Histogram;
use reth::rpc::api::eth::helpers::pending_block;
use reth_chain_state::ExecutedBlock;
use reth_evm::block::BlockExecutionError;
use reth_node_api::{BuiltPayloadExecutedBlock, Events};
use reth_optimism_node::{OpBuiltPayload, OpEngineTypes};
use reth_optimism_primitives::OpPrimitives;
use reth_trie_common::HashedPostStateSorted;
use std::sync::{Arc, LazyLock};
use tokio::sync::{
    broadcast::{self, Sender},
    mpsc::UnboundedSender,
};
use tokio_stream::wrappers::BroadcastStream;
use tracing::{error, trace};

const TARGET: &str = "flashblocks::processor";

static VALIDATE_BAL_HISTOGRAM: LazyLock<Histogram> =
    LazyLock::new(|| metrics::histogram!("flashblocks.validate", "access_list" => "true"));

static VALIDATE_LEGACY_HISTOGRAM: LazyLock<Histogram> =
    LazyLock::new(|| metrics::histogram!("flashblocks.validate", "access_list" => "false"));

// ---------------------------------------------------------------------------
// PendingBlockNotification
// ---------------------------------------------------------------------------

/// A notification of a validated pending block.
#[derive(Debug, Clone)]
pub enum PendingBlockNotification {
    /// Validated from the P2P network by a [`PendingPayloadProcessor`].
    External {
        block: ExecutedBlock<OpPrimitives>,
        flashblock: FlashblocksPayloadV1,
    },
    /// Built locally by this node's payload builder.
    Internal {
        block: ExecutedBlock<OpPrimitives>,
        authorized: AuthorizedPayload<FlashblocksPayloadV1>,
    },
}

impl PendingBlockNotification {
    pub fn block(&self) -> &ExecutedBlock<OpPrimitives> {
        match self {
            PendingBlockNotification::External { block, .. } => block,
            PendingBlockNotification::Internal { block, .. } => block,
        }
    }

    pub fn flashblock(&self) -> &FlashblocksPayloadV1 {
        match self {
            PendingBlockNotification::External { flashblock, .. } => flashblock,
            PendingBlockNotification::Internal { block, authorized } => authorized.msg(),
        }
    }
}

// ---------------------------------------------------------------------------
// FlashblockProcessor
// ---------------------------------------------------------------------------

/// Single subscriber for all pending block state mutations.
///
/// Owns `flashblocks`, `pending_block`, and `payload_events`.
/// Driven by [`PendingBlockNotification`]s from both the P2P validation path
/// and the local builder path.
pub struct FlashblockProcessor {
    current_flashblock: Option<PayloadId>,
    pending_block: tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
    payload_events: broadcast::Sender<Events<OpEngineTypes>>,
    notification_rx: Option<broadcast::Receiver<PendingBlockNotification>>,
}

impl std::fmt::Debug for FlashblockProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlashblockProcessor")
            .finish_non_exhaustive()
    }
}

impl FlashblockProcessor {
    pub fn new(
        pending_block: tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
        notifications: broadcast::Sender<PendingBlockNotification>,
        payload_events: broadcast::Sender<Events<OpEngineTypes>>,
    ) -> Self {
        Self {
            current_flashblock: None,
            pending_block,
            notification_rx: Some(notifications.subscribe()),
            payload_events,
        }
    }

    /// Drives the notification listener until all senders are dropped.
    pub async fn spawn_nofification_handler(mut self) {
        let rx = self.notification_rx.take().expect("run() called twice");
        let mut stream = BroadcastStream::new(rx).filter_map(|r| futures::future::ready(r.ok()));

        while let Some(notification) = stream.next().await {
            // Common: update pending block
            self.pending_block
                .send_replace(Some(notification.block().clone()));

            let flashblock = notification.flashblock();
            let payload = notification.block();

            match notification {
                PendingBlockNotification::External { .. } => {
                    self.broadcast_payload(payload.clone(), flashblock.clone());
                    self.broadcast_pending_block(payload.clone());
                }
                PendingBlockNotification::Internal { .. } => {
                    self.broadcast_pending_block(payload.clone());
                }
            }
        }
    }

    fn broadcast_payload(
        &self,
        payload: ExecutedBlock<OpPrimitives>,
        flashblock: FlashblocksPayloadV1,
    ) {
        self.payload_events
            .send(Events::BuiltPayload(OpBuiltPayload::new(
                flashblock.payload_id,
                Arc::new(payload.sealed_block().clone()),
                U256::ZERO,
                Some(BuiltPayloadExecutedBlock {
                    recovered_block: payload.recovered_block.clone(),
                    execution_output: payload.execution_output.clone(),
                    hashed_state: either::Either::Right(payload.hashed_state()),
                    trie_updates: either::Either::Right(payload.trie_updates()),
                }),
            )));
    }

    fn broadcast_pending_block(&self, block: ExecutedBlock<OpPrimitives>) {
        self.pending_block.send_replace(Some(block));
    }
}

/// Processes flashblocks for a single epoch (payload_id).
pub struct PendingPayloadProcessor<F> {
    validate: F,
    pending: Vec<FlashblocksPayloadV1>,
}

impl<F> PendingPayloadProcessor<F>
where
    F: FnMut(
            &FlashblocksPayloadV1,
            Option<&ExecutedBlock<OpPrimitives>>,
        ) -> Result<ExecutedBlock<OpPrimitives>, BlockExecutionError>
        + Send
        + 'static,
{
    pub fn new(validate: F, base: FlashblocksPayloadV1) -> Self {
        Self {
            validate,
            pending: vec![base],
        }
    }

    /// Consumes flashblocks until the stream ends or the payload_id changes.
    ///
    /// Calls `on_validated` with each successfully validated [`ExecutedBlock`].
    pub fn run(
        mut self,
        rx: broadcast::Receiver<FlashblocksPayloadV1>,
        notifications_sender: broadcast::Sender<PendingBlockNotification>,
    ) {
        let handle = tokio::runtime::Handle::current();
        let mut pending_executed_block = None::<ExecutedBlock<OpPrimitives>>;
        let payload_id = self.pending.first().unwrap().payload_id;

        handle.block_on(async {
            let mut last_index = None::<u64>;

            let base = stream::iter(self.pending);
            let st = BroadcastStream::new(rx)
                .filter_map(|r| future::ready(r.ok()))
                .take_while(move |fb| future::ready(fb.payload_id == payload_id));

            let stream = base.chain(st).filter(move |fb| {
                let accept = !last_index.is_some_and(|l| fb.index <= l);
                if accept {
                    last_index = Some(fb.index);
                }
                future::ready(accept)
            });

            futures::pin_mut!(stream);

            while let Some(fb) = stream.next().await {
                let index = fb.index;
                let has_bal = fb.diff.access_list_data.is_some();
                let tx_count = fb.diff.transactions.len();

                let histogram = if has_bal {
                    VALIDATE_BAL_HISTOGRAM.clone()
                } else {
                    VALIDATE_LEGACY_HISTOGRAM.clone()
                };

                let path = if has_bal { "bal" } else { "legacy" };

                crate::metrics::metered_fn(
                    tracing::trace_span!(
                        target: "flashblocks::processor",
                        "validate",
                        id = %payload_id,
                        index,
                        path,
                        tx_count,
                        duration_ms = tracing::field::Empty,
                    ),
                    histogram,
                    |_span| (self.validate)(&fb, pending_executed_block.as_ref()),
                )
                .map(|executed_block| {
                    notifications_sender
                        .send(PendingBlockNotification::External {
                            block: executed_block.clone(),
                            flashblock: fb.clone(),
                        })
                        .ok();

                    pending_executed_block = Some(executed_block);

                    trace!(
                        target: "flashblocks::processor",
                        id = %payload_id, index, path,
                        tx_count,
                        "validated flashblock"
                    );
                })
                .map_err(|e| {
                    error!(
                        target: "flashblocks::processor",
                        id = %payload_id, index, path,
                        error = ?e, "failed to validate flashblock"
                    );
                })
                .ok();
            }
        });
    }
}
