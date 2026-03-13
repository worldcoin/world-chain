//! Canon-aware flashblock event stream.
//!
//! Merges a raw flashblock stream with canonical chain notifications, yielding
//! [`FlashblocksEvent::Pending`] only when the flashblock's epoch parent matches
//! the current canonical tip, and [`FlashblocksEvent::Canon`] whenever the tip
//! changes.

use flashblocks_primitives::primitives::FlashblocksPayloadV1;
use futures::{
    future::{self, Either},
    stream::{self, PollNext},
    Stream, StreamExt,
};
use reth::{
    api::NodePrimitives, payload::PayloadId, providers::CanonStateSubscriptions,
    rpc::types::BlockNumHash,
};
use std::{
    fmt::Debug,
    marker::PhantomData,
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
    Pending(FlashblocksPayloadV1),
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
        WorldChainEvent::Chain(value)
    }
}

impl<T> From<FlashblocksPayloadV1> for WorldChainEvent<T> {
    fn from(value: FlashblocksPayloadV1) -> Self {
        WorldChainEvent::Chain(ChainEvent::Pending(value))
    }
}

impl<T> From<BlockNumHash> for WorldChainEvent<T> {
    fn from(value: BlockNumHash) -> Self {
        WorldChainEvent::Chain(ChainEvent::Canon(value))
    }
}

/// Events yielded by [`ChainEventsStream`].
#[derive(Clone, Debug)]
pub enum WorldChainEvent<T> {
    /// An Event emitted when executable pending flashblocks are observed.
    Chain(ChainEvent),
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
    ) -> WorldChainEventsStream<T>
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
            .map(Into::into);

        let canon = provider
            .canonical_state_stream()
            .map(|n| WorldChainEvent::Chain(ChainEvent::Canon(n.tip().num_hash().into())));

        Self::new_with_hook(
            move |_: &WorldChainEvent<T>| None,
            flashblocks.boxed(),
            canon.boxed(),
        )
    }

    /// Creates a new [`WorldChainEventsStream`] mapping all `ChainEvent`s through `hook` before yielding them.
    pub fn new_with_hook<'a, F>(
        mut hook: F,
        st_0: WorldChainEventNotificationsStream<T>,
        st_1: WorldChainEventNotificationsStream<T>,
    ) -> Self
    where
        F: FnMut(&WorldChainEvent<T>) -> Option<WorldChainEvent<T>> + Send + 'static,
    {
        let merged = merge_flashblocks_with_canon(st_0, st_1);
        let st = merged
            .flat_map(move |event| {
                let extra = hook(&event);
                stream::iter(std::iter::once(event).chain(extra))
            })
            .boxed();

        Self { st: Box::pin(st) }
    }
}

impl<T: Clone + Unpin + Send> Stream for WorldChainEventsStream<T> {
    type Item = WorldChainEvent<T>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.st.as_mut().poll_next(cx)
    }
}

/// Merges a flashblock stream with a canonical state notification stream,
/// using [`BufferState`] to buffer, order, and gate flashblocks by the
/// canonical tip. Only yields flashblocks whose epoch parent is canonical.
fn merge_flashblocks_with_canon<T: Clone + Send + 'static>(
    st_0: WorldChainEventNotificationsStream<T>,
    st_1: WorldChainEventNotificationsStream<T>,
) -> WorldChainEventNotificationsStream<T> {
    futures::stream::select_with_strategy(
        st_0.map(Either::Left),
        st_1.map(Either::Right),
        |_: &mut ()| -> PollNext { PollNext::Left },
    )
    .scan(BufferState::default(), |state, event| {
        futures::future::ready(Some(match event {
            Either::Left(WorldChainEvent::Chain(ChainEvent::Pending(fb))) => state.advance(fb),
            Either::Right(WorldChainEvent::Chain(ChainEvent::Canon(tip))) => state.process(tip),
            _ => vec![],
        }))
    })
    .flat_map(stream::iter)
    .boxed()
}
// ---------------------------------------------------------------------------
// Type-state markers
// ---------------------------------------------------------------------------

/// No active epoch — waiting for a base flashblock.
pub struct Idle;
/// Have a payload_id but parent is not yet canonical — buffering only.
pub struct Unanchored;
/// Parent is canonical — can drain contiguous flashblocks.
pub struct Anchored;

const MAX_FLASHBLOCK_INDEX: usize = 100;

// ---------------------------------------------------------------------------
// BufferedFlashblocks
// ---------------------------------------------------------------------------

/// Tracks and buffers flashblocks for the current epoch with type-state
/// transitions governing when flashblocks may be drained.
///
/// - [`Idle`]: No active epoch. Accepts a base flashblock to start one.
/// - [`Unanchored`]: Epoch started but parent not yet canonical. Buffers only.
/// - [`Anchored`]: Parent is canonical. Can drain contiguous flashblocks.
pub struct BufferedFlashblocks<S = Idle> {
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
    _state: PhantomData<S>,
}

/// A flashblock drained from the buffer, ready for the coordinator.
pub struct PendingFlashblockEvent {
    /// Resolve when the flashblock is in the in-memory tree.
    /// Dropping without sending signals cancellation.
    pub tx: oneshot::Sender<()>,
    /// The drained flashblock payload.
    pub flashblock: FlashblocksPayloadV1,
}

// -- Shared methods (all states) --

impl<S> BufferedFlashblocks<S> {
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

    fn set_canon_tip(&mut self, tip: BlockNumHash) {
        self.canon_tip = Some(tip);
    }

    /// Reset to [`Idle`], discarding all buffered flashblocks.
    fn reset(self) -> BufferedFlashblocks<Idle> {
        BufferedFlashblocks {
            parent_num_hash: BlockNumHash::default(),
            payload_id: PayloadId::default(),
            timestamp: 0,
            cursor: 0,
            buffer: Vec::new(),
            canon_tip: self.canon_tip,
            _state: PhantomData,
        }
    }
}

// -- Idle --

impl Default for BufferedFlashblocks<Idle> {
    fn default() -> Self {
        Self {
            parent_num_hash: BlockNumHash::default(),
            payload_id: PayloadId::default(),
            timestamp: 0,
            cursor: 0,
            buffer: Vec::new(),
            canon_tip: None,
            _state: PhantomData,
        }
    }
}

impl BufferedFlashblocks<Idle> {
    /// Accept a base flashblock and start a new epoch.
    /// Returns `None` if the flashblock has no base or is stale.
    fn accept_base(
        mut self,
        fb: FlashblocksPayloadV1,
    ) -> Option<Either<BufferedFlashblocks<Unanchored>, BufferedFlashblocks<Anchored>>> {
        let base = fb.base.as_ref()?;

        if let Some(tip) = &self.canon_tip {
            if base.timestamp <= tip.number {
                return None;
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

        let anchored = self
            .canon_tip
            .is_some_and(|tip| tip == self.parent_num_hash);

        if anchored {
            Some(Either::Right(BufferedFlashblocks {
                parent_num_hash: self.parent_num_hash,
                payload_id: self.payload_id,
                timestamp: self.timestamp,
                cursor: self.cursor,
                buffer: self.buffer,
                canon_tip: self.canon_tip,
                _state: PhantomData,
            }))
        } else {
            Some(Either::Left(BufferedFlashblocks {
                parent_num_hash: self.parent_num_hash,
                payload_id: self.payload_id,
                timestamp: self.timestamp,
                cursor: self.cursor,
                buffer: self.buffer,
                canon_tip: self.canon_tip,
                _state: PhantomData,
            }))
        }
    }
}

// -- Unanchored --

impl BufferedFlashblocks<Unanchored> {
    /// If the canon tip matches our parent, transition to [`Anchored`].
    fn try_anchor(self) -> Either<BufferedFlashblocks<Unanchored>, BufferedFlashblocks<Anchored>> {
        if self
            .canon_tip
            .is_some_and(|tip| tip == self.parent_num_hash)
        {
            Either::Right(BufferedFlashblocks {
                parent_num_hash: self.parent_num_hash,
                payload_id: self.payload_id,
                timestamp: self.timestamp,
                cursor: self.cursor,
                buffer: self.buffer,
                canon_tip: self.canon_tip,
                _state: PhantomData,
            })
        } else {
            Either::Left(self)
        }
    }
}

// -- Anchored --

impl BufferedFlashblocks<Anchored> {
    /// Drain all contiguous flashblocks from the cursor, yielding a
    /// [`PendingFlashblockEvent`] for each. Takes ownership via `.take()`.
    fn drain_contiguous(&mut self) -> Vec<PendingFlashblockEvent> {
        let mut events = Vec::new();
        while let Some(Some(_)) = self.buffer.get(self.cursor) {
            let fb = self.buffer[self.cursor].take().unwrap();
            self.cursor += 1;
            let (tx, _rx) = oneshot::channel();
            events.push(PendingFlashblockEvent { tx, flashblock: fb });
        }
        events
    }
}

// ---------------------------------------------------------------------------
// Runtime state enum (for scan closures)
// ---------------------------------------------------------------------------

enum BufferState {
    Idle(BufferedFlashblocks<Idle>),
    Unanchored(BufferedFlashblocks<Unanchored>),
    Anchored(BufferedFlashblocks<Anchored>),
}

impl Default for BufferState {
    fn default() -> Self {
        Self::Idle(BufferedFlashblocks::default())
    }
}

impl BufferState {
    /// Advance the buffer with a new flashblock. Handles epoch starts,
    /// inserts, and state transitions. Returns events to yield downstream.
    fn advance<T: Clone + Send>(&mut self, fb: FlashblocksPayloadV1) -> Vec<WorldChainEvent<T>> {
        if fb.base.is_some() {
            // Base flashblock → reset and start new epoch
            let idle = match std::mem::take(self) {
                Self::Idle(b) => b,
                Self::Unanchored(b) => b.reset(),
                Self::Anchored(b) => b.reset(),
            };

            match idle.accept_base(fb) {
                Some(Either::Right(mut anchored)) => {
                    let events = Self::drain_to_events(&mut anchored);
                    *self = Self::Anchored(anchored);
                    events
                }
                Some(Either::Left(unanchored)) => {
                    *self = Self::Unanchored(unanchored);
                    vec![]
                }
                None => vec![], // stale, discarded
            }
        } else {
            // Non-base → insert into current buffer
            match self {
                Self::Idle(_) => {} // no epoch, ignore
                Self::Unanchored(b) => {
                    b.insert(&fb);
                }
                Self::Anchored(b) => {
                    b.insert(&fb);
                }
            }

            // If anchored, drain any newly contiguous flashblocks
            if let Self::Anchored(anchored) = self {
                Self::drain_to_events(anchored)
            } else {
                vec![]
            }
        }
    }

    /// Process a new canonical tip. Updates state and potentially anchors
    /// the buffer, draining any contiguous flashblocks.
    fn process<T: Clone + Send>(&mut self, tip: BlockNumHash) -> Vec<WorldChainEvent<T>> {
        // Always emit the canon event
        let mut events: Vec<WorldChainEvent<T>> = vec![tip.into()];

        match self {
            Self::Idle(b) => b.set_canon_tip(tip),
            Self::Unanchored(_) => {
                // Try to anchor
                let unanchored = match std::mem::take(self) {
                    Self::Unanchored(mut b) => {
                        b.set_canon_tip(tip);
                        b
                    }
                    _ => unreachable!(),
                };

                match unanchored.try_anchor() {
                    Either::Right(mut anchored) => {
                        events.extend(Self::drain_to_events(&mut anchored));
                        *self = Self::Anchored(anchored);
                    }
                    Either::Left(still_unanchored) => {
                        *self = Self::Unanchored(still_unanchored);
                    }
                }
            }
            Self::Anchored(b) => b.set_canon_tip(tip),
        }

        events
    }

    fn drain_to_events<T: Clone + Send>(
        anchored: &mut BufferedFlashblocks<Anchored>,
    ) -> Vec<WorldChainEvent<T>> {
        anchored
            .drain_contiguous()
            .into_iter()
            .map(|e| WorldChainEvent::<T>::from(e.flashblock))
            .collect()
    }
}
