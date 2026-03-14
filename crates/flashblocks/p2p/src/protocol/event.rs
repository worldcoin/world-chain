//! Canon-aware flashblock event stream.
//!
//! Merges a raw flashblock stream with canonical chain notifications, yielding
//! [`FlashblocksEvent::Pending`] only when the flashblock's epoch parent matches
//! the current canonical tip, and [`FlashblocksEvent::Canon`] whenever the tip
//! changes.

use flashblocks_primitives::primitives::FlashblocksPayloadV1;
use futures::{
    Stream, StreamExt,
    future::{self},
    stream::{self, PollNext},
};
use reth::{
    api::NodePrimitives, payload::PayloadId, providers::CanonStateSubscriptions,
    rpc::types::BlockNumHash,
};
use std::{
    collections::VecDeque,
    fmt::Debug,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::{broadcast, oneshot};
use tokio_stream::wrappers::BroadcastStream;

#[derive(Clone, Debug)]
pub enum ChainEvent {
    /// A new canonical tip has been observed. Consumers should clear any
    /// pending flashblocks that are stale relative to the new tip.
    Canon(BlockNumHash),
    /// A flashblock has been received whose epoch parent matches the current
    /// canonical tip. Consumers can treat this as a "pending" event and buffer
    /// it until the next Canon event confirms it's ready to be processed.
    Pending(Box<FlashblocksPayloadV1>),
}

impl ChainEvent {
    pub fn is_canon(&self) -> bool {
        matches!(self, ChainEvent::Canon(_))
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, ChainEvent::Pending(_))
    }
}

impl From<ChainEvent> for WorldChainEvent<ChainEvent> {
    fn from(value: ChainEvent) -> Self {
        WorldChainEvent::Chain(Box::new(value))
    }
}

impl<T> From<FlashblocksPayloadV1> for WorldChainEvent<T> {
    fn from(value: FlashblocksPayloadV1) -> Self {
        WorldChainEvent::Chain(Box::new(ChainEvent::Pending(Box::new(value))))
    }
}

impl<T> From<BlockNumHash> for WorldChainEvent<T> {
    fn from(value: BlockNumHash) -> Self {
        WorldChainEvent::Chain(Box::new(ChainEvent::Canon(value)))
    }
}

/// Events yielded by [`ChainEventsStream`].
#[derive(Clone, Debug)]
pub enum WorldChainEvent<T> {
    /// An Event emitted when executable pending flashblocks are observed.
    Chain(Box<ChainEvent>),
    /// A Event emitted by any source.
    Event(T),
}

/// Convenience alias: a [`ChainEvent`] carrying a flashblocks payload.
pub type WorldChainEventNotificationsStream<T> =
    Pin<Box<dyn Stream<Item = WorldChainEvent<T>> + Send>>;

/// A stream of [`ChainEvent`]s that merges flashblocks with canonical chain
/// notifications.
///
/// Follows the same pattern as reth's `CanonStateNotificationStream` — wraps an
/// inner stream and handles lag transparently.
///
/// A [`ChainEvent::Pending`] is emitted only when the flashblock's epoch parent
/// matches the canonical tip. Stale flashblocks are silently discarded via
/// [`PendingCursor::try_advance`]. A [`ChainEvent::Canon`] is emitted on every
/// canonical tip change so consumers can clear pending state.
/// A stream of [`WorldChainEvent`]s that merges flashblocks with canonical
/// chain notifications, reducing them through a [`BufferedCursor`](sealed::BufferedCursor)
/// state machine.
///
/// Implements [`Stream`] directly — polls the merged inner streams, feeds
/// each [`ChainEvent`] through the cursor's `reduce`, and yields the output
/// events one at a time.
#[pin_project::pin_project]
pub struct WorldChainEventsStream<T> {
    #[pin]
    st: WorldChainEventNotificationsStream<T>,
}

impl<T: Send + Clone + Unpin + 'static> WorldChainEventsStream<T> {
    /// Creates a new [`WorldChainEventsStream`] by merging a flashblock
    /// receiver with canonical chain notifications from `provider`.
    pub fn new<P, N: NodePrimitives>(
        provider: P,
        rx: broadcast::Receiver<FlashblocksPayloadV1>,
    ) -> Self
    where
        P: CanonStateSubscriptions<Primitives = N> + Clone + Send + Sync + 'static,
    {
        let flashblocks = BroadcastStream::new(rx)
            .filter_map(|x| {
                future::ready(match x {
                    Ok(fb) => Some(fb),
                    Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                        tracing::warn!(missed = n, "flashblocks broadcast receiver lagged");
                        None
                    }
                })
            })
            .map(|fb| ChainEvent::Pending(Box::new(fb)));

        let canon = provider
            .canonical_state_stream()
            .map(|n| ChainEvent::Canon(n.tip().num_hash()));

        Self::new_with_hook(
            move |_: &WorldChainEvent<T>| None,
            flashblocks.boxed(),
            canon.boxed(),
        )
    }

    /// Creates a new [`WorldChainEventsStream`] with a hook that can inject
    /// additional events after each yielded event.
    pub fn new_with_hook<F>(
        mut hook: F,
        flashblocks: Pin<Box<dyn Stream<Item = ChainEvent> + Send>>,
        canon: Pin<Box<dyn Stream<Item = ChainEvent> + Send>>,
    ) -> Self
    where
        F: FnMut(&WorldChainEvent<T>) -> Option<WorldChainEvent<T>> + Send + 'static,
    {
        let merged =
            futures::stream::select_with_strategy(flashblocks, canon, |_: &mut ()| -> PollNext {
                PollNext::Left
            });

        // Fold through the BufferedFlashblocks reducer, then flat_map output.
        let st = merged
            .scan(BufferedFlashblocks::default(), |cursor, event| {
                cursor.step(event);
                let events: Vec<WorldChainEvent<T>> = cursor
                    .by_ref()
                    .map(|ce| WorldChainEvent::Chain(Box::new(ce)))
                    .collect();
                future::ready(Some(events))
            })
            .flat_map(stream::iter)
            .flat_map(move |event| {
                let extra = hook(&event);
                stream::iter(std::iter::once(event).chain(extra))
            })
            .boxed();

        Self { st }
    }
}

impl<T: Clone + Unpin + Send> Stream for WorldChainEventsStream<T> {
    type Item = WorldChainEvent<T>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.st.as_mut().poll_next(cx)
    }
}
// ---------------------------------------------------------------------------
// BufferedFlashblocks — stateful reducer with Extend + Iterator
// ---------------------------------------------------------------------------

const MAX_FLASHBLOCK_INDEX: usize = 100;

/// Internal phase of the buffer state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// No active epoch.
    Uninitialized,
    /// Epoch started but parent is not yet canonical.
    Pending,
    /// Parent is canonical — flashblocks can be drained.
    Executable,
}

/// Buffers flashblocks for the current epoch, gating output on canonical tip.
///
/// Implements [`Extend<ChainEvent>`] to accept input events and
/// [`Iterator<Item = ChainEvent>`] to drain output events. The internal
/// state machine transitions between `Uninitialized`, `Pending`, and
/// `Executable` phases automatically.
pub struct BufferedFlashblocks {
    phase: Phase,
    /// The parent block this epoch builds on.
    parent_num_hash: BlockNumHash,
    /// Current epoch payload identifier.
    payload_id: PayloadId,
    /// Timestamp of the current epoch.
    timestamp: u64,
    /// Drain watermark — next index to drain from.
    cursor: usize,
    /// Sparse buffer indexed by flashblock sequence number.
    buffer: Vec<Option<FlashblocksPayloadV1>>,
    /// Most recent canonical tip.
    canon_tip: Option<BlockNumHash>,
    /// Output events ready to be yielded by the iterator.
    output: VecDeque<ChainEvent>,
}

/// A flashblock drained from the buffer, ready for the coordinator.
pub struct PendingFlashblockEvent {
    /// Resolve when the flashblock is in the in-memory tree.
    pub tx: oneshot::Sender<()>,
    /// The drained flashblock payload.
    pub flashblock: FlashblocksPayloadV1,
}

impl Default for BufferedFlashblocks {
    fn default() -> Self {
        Self {
            phase: Phase::Uninitialized,
            parent_num_hash: BlockNumHash::default(),
            payload_id: PayloadId::default(),
            timestamp: 0,
            cursor: 0,
            buffer: Vec::new(),
            canon_tip: None,
            output: VecDeque::new(),
        }
    }
}

impl BufferedFlashblocks {
    /// Insert a flashblock at its index. Returns `false` if the payload_id
    /// doesn't match, the index exceeds the maximum, or the slot is occupied.
    fn insert(&mut self, fb: &FlashblocksPayloadV1) -> bool {
        if fb.payload_id != self.payload_id {
            return false;
        }
        let idx = fb.index as usize;
        if idx > MAX_FLASHBLOCK_INDEX {
            return false;
        }
        let len = self.buffer.len();
        self.buffer.resize_with(len.max(idx + 1), || None);
        if self.buffer[idx].is_some() {
            return false;
        }
        self.buffer[idx] = Some(fb.clone());
        true
    }

    /// Reset to uninitialized, discarding all buffered flashblocks.
    fn reset(&mut self) {
        self.phase = Phase::Uninitialized;
        self.parent_num_hash = BlockNumHash::default();
        self.payload_id = PayloadId::default();
        self.timestamp = 0;
        self.cursor = 0;
        self.buffer.clear();
        // canon_tip is preserved
    }

    /// Try to start a new epoch from a base flashblock.
    fn accept_base(&mut self, fb: FlashblocksPayloadV1) -> bool {
        let Some(base) = fb.base.as_ref() else {
            return false;
        };

        // Stale check
        if let Some(tip) = &self.canon_tip {
            if base.timestamp <= tip.number {
                return false;
            }
        }

        self.parent_num_hash = BlockNumHash {
            number: base.block_number.saturating_sub(1),
            hash: base.parent_hash,
        };
        self.payload_id = fb.payload_id;
        self.timestamp = base.timestamp;
        self.cursor = 0;
        self.buffer.clear();
        self.insert(&fb);

        self.phase = if self
            .canon_tip
            .is_some_and(|tip| tip == self.parent_num_hash)
        {
            Phase::Executable
        } else {
            Phase::Pending
        };

        true
    }

    /// If pending and canon tip matches parent, transition to executable.
    fn try_anchor(&mut self) {
        if self.phase == Phase::Pending
            && self
                .canon_tip
                .is_some_and(|tip| tip == self.parent_num_hash)
        {
            self.phase = Phase::Executable;
        }
    }

    /// Drain all contiguous flashblocks from the cursor into the output queue.
    fn drain_contiguous(&mut self) {
        if self.phase != Phase::Executable {
            return;
        }
        while let Some(Some(_)) = self.buffer.get(self.cursor) {
            let fb = self.buffer[self.cursor].take().unwrap();
            self.cursor += 1;
            self.output.push_back(ChainEvent::Pending(Box::new(fb)));
        }
    }

    /// Process a single input event, updating state and buffering output.
    fn step(&mut self, event: ChainEvent) {
        match event {
            ChainEvent::Pending(fb) => {
                if fb.base.is_some() {
                    // New epoch — reset and try to accept
                    self.reset();
                    self.accept_base(*fb);
                    self.drain_contiguous();
                } else {
                    // Non-base — insert into current buffer if we have an epoch
                    if self.phase != Phase::Uninitialized {
                        self.insert(&fb);
                        self.drain_contiguous();
                    }
                }
            }
            ChainEvent::Canon(tip) => {
                self.canon_tip = Some(tip);
                self.output.push_back(ChainEvent::Canon(tip));
                self.try_anchor();
                self.drain_contiguous();
            }
        }
    }
}

impl Extend<ChainEvent> for BufferedFlashblocks {
    fn extend<I: IntoIterator<Item = ChainEvent>>(&mut self, iter: I) {
        for event in iter {
            self.step(event);
        }
    }
}

impl Iterator for BufferedFlashblocks {
    type Item = ChainEvent;

    fn next(&mut self) -> Option<Self::Item> {
        self.output.pop_front()
    }
}
