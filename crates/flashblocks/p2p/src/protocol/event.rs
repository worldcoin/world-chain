//! Canon-aware flashblock event stream.
//!
//! Merges a raw flashblock stream with canonical chain notifications, yielding
//! [`FlashblocksEvent::Pending`] only when the flashblock's epoch parent matches
//! the current canonical tip, and [`FlashblocksEvent::Canon`] whenever the tip
//! changes.

use flashblocks_primitives::primitives::FlashblocksPayloadV1;
use futures::{
    future::{self, Either},
    stream, Stream, StreamExt,
};
use reth::{
    api::NodePrimitives,
    payload::PayloadId,
    providers::{CanonStateNotificationStream, CanonStateSubscriptions},
    rpc::types::BlockNumHash,
};
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

/// Events yielded by [`ChainEventsStream`].
#[derive(Clone, Debug)]
pub enum ChainEvent<T> {
    /// A pending flashblock confirmed to build on the canonical tip.
    Pending(T),
    /// The canonical tip changed — consumers should clear stale pending state.
    Canon(BlockNumHash),
}

/// Convenience alias: a [`ChainEvent`] carrying a flashblocks payload.
pub type FlashblocksEvent = ChainEvent<FlashblocksPayloadV1>;

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
pub struct ChainEventsStream<T> {
    st: Pin<Box<dyn Stream<Item = ChainEvent<T>> + Send>>,
}

/// Convenience alias: a [`ChainEventsStream`] carrying flashblocks payloads.
pub type WorldChainEventsStream = ChainEventsStream<FlashblocksPayloadV1>;

impl WorldChainEventsStream {
    /// Creates a new [`WorldChainEventsStream`] by merging a flashblock
    /// receiver with canonical chain notifications from `provider`.
    pub fn new<P: CanonStateSubscriptions + ?Sized>(
        flashblocks_rx: broadcast::Receiver<FlashblocksPayloadV1>,
        provider: &P,
    ) -> Self {
        let flashblocks =
            BroadcastStream::new(flashblocks_rx).filter_map(|x| future::ready(x.ok()));

        let canon = provider.canonical_state_stream();

        Self {
            st: stream_select_contiguous_with_canon(flashblocks, canon),
        }
    }
}

/// Merges a flashblock stream with a canonical state notification stream,
/// yielding [`FlashblocksEvent`]s gated by the canonical tip.
///
/// Flashblocks are only emitted as [`FlashblocksEvent::Pending`] when their
/// epoch parent matches the current canonical tip. When a new canonical tip
/// arrives and a stored flashblock becomes ready, both [`FlashblocksEvent::Canon`]
/// and [`FlashblocksEvent::Pending`] are emitted in that order.
///
/// The inner state machine uses `scan` over a merged `select` of both input
/// streams, accumulating zero or more events per input item into a `Vec` that
/// is flattened back into the output stream via `flat_map(stream::iter)`.
fn stream_select_contiguous_with_canon<N: NodePrimitives>(
    fb: impl Stream<Item = FlashblocksPayloadV1> + Send + 'static,
    canon: CanonStateNotificationStream<N>,
) -> Pin<Box<dyn Stream<Item = ChainEvent<FlashblocksPayloadV1>> + Send>> {
    // Tag each source with `Either` so the merged stream can distinguish them.
    futures::stream::select(fb.map(Either::Left), canon.map(Either::Right))
        .scan(
            // Scan state:
            //   tip    — the most recent canonical tip (None until first canon event)
            //   cursor — tracks epoch position; enforces flashblock adjacency
            //   latest — most recently accepted flashblock, buffered until the
            //            canonical tip confirms it can be yielded
            (
                None::<BlockNumHash>,
                PendingCursor::default(),
                None::<FlashblocksPayloadV1>,
            ),
            |(tip, cursor, latest), event| {
                let events: Vec<ChainEvent<FlashblocksPayloadV1>> = match event {
                    // ── Flashblock arrived ──
                    //
                    // `try_advance` atomically validates adjacency (sequential
                    // index within an epoch, or a new epoch via base flashblock)
                    // and rejects flashblocks stale relative to the known tip.
                    // Only if it succeeds do we buffer the flashblock.
                    Either::Left(fb) if cursor.try_advance(&fb, tip.as_ref()) => {
                        *latest = Some(fb);
                        // If the canonical tip is already known and the cursor's
                        // epoch parent matches it, emit immediately. Otherwise
                        // buffer — a later Canon event will flush it.
                        if tip.as_ref().is_some_and(|t| cursor.is_ready(t)) {
                            latest
                                .take()
                                .into_iter()
                                .map(FlashblocksEvent::Pending)
                                .collect()
                        } else {
                            vec![]
                        }
                    }
                    // Flashblock failed adjacency or staleness check — drop it.
                    Either::Left(_) => vec![],

                    // ── Canonical tip changed ──
                    //
                    // Always notify consumers so they can clear stale pending
                    // state. If the new tip matches the cursor's epoch parent,
                    // also flush the buffered flashblock (Canon first, then
                    // Pending).
                    Either::Right(n) => {
                        let num_hash = n.tip().num_hash();
                        *tip = Some(num_hash);
                        let mut events = vec![FlashblocksEvent::Canon(num_hash)];
                        if cursor.is_ready(&num_hash) {
                            if let Some(fb) = latest.take() {
                                events.push(FlashblocksEvent::Pending(fb));
                            }
                        }
                        events
                    }
                };
                // Wrapping in `Some` keeps the stream alive — the inner `Vec`
                // may be empty (no events to yield this tick), which `flat_map`
                // handles by producing nothing.
                future::ready(Some(events))
            },
        )
        // Each scan step produces a `Vec<FlashblocksEvent>`; flatten into
        // individual items so the outer stream yields one event at a time.
        .flat_map(stream::iter)
        .boxed()
}

impl std::fmt::Debug for WorldChainEventsStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldChainEventsStream")
            .field("st", &"<stream>")
            .finish_non_exhaustive()
    }
}

impl Stream for WorldChainEventsStream {
    type Item = FlashblocksEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.st.as_mut().poll_next(cx)
    }
}

// ---------------------------------------------------------------------------
// Cursor
// ---------------------------------------------------------------------------

/// Tracks position of Flashblocks w.r.t. a Blocks Epoch.
#[derive(Clone, Copy, Debug, Default)]
struct PendingCursor {
    /// The parent block that this epoch builds on.
    parent_num_hash: BlockNumHash,
    /// The payload identifier for the current epoch.
    payload_id: PayloadId,
    /// The latest flashblock index within the current epoch.
    index: usize,
}

impl PendingCursor {
    /// Returns `true` when the epoch parent matches the canonical tip exactly.
    fn is_ready(&self, canon_tip: &BlockNumHash) -> bool {
        self.parent_num_hash == *canon_tip
    }

    /// Returns `true` when this epoch is at least as recent as `canon_tip` —
    /// either matching it or building on a block canon hasn't reached yet.
    fn is_new(&self, canon_tip: &BlockNumHash) -> bool {
        self.is_ready(canon_tip) || self.parent_num_hash.number > canon_tip.number
    }

    /// Advances the cursor to track the given flashblock.
    ///
    /// Returns `true` if the flashblock is adjacent — either a base flashblock
    /// starting a new epoch, or the next sequential index within the current
    /// epoch. Returns `false` (and leaves the cursor unchanged) on gaps or
    /// payload-id mismatches.
    fn advance(&mut self, flashblock: &FlashblocksPayloadV1) -> bool {
        if let Some(base) = &flashblock.base {
            // New epoch — always adjacent.
            self.parent_num_hash = BlockNumHash {
                number: base.block_number - 1,
                hash: base.parent_hash,
            };
            self.payload_id = flashblock.payload_id;
            self.index = flashblock.index as usize;
            true
        } else if flashblock.payload_id == self.payload_id
            && flashblock.index as usize == self.index + 1
        {
            // Same epoch, next sequential index.
            self.index += 1;
            true
        } else {
            false
        }
    }

    /// Speculatively advances the cursor. Returns `false` (and leaves the
    /// cursor unchanged) when the flashblock is not adjacent or is stale
    /// relative to `tip`.
    fn try_advance(&mut self, fb: &FlashblocksPayloadV1, tip: Option<&BlockNumHash>) -> bool {
        let mut next = *self;
        if !next.advance(fb) {
            return false;
        }
        if tip.is_some_and(|t| !next.is_new(t)) {
            return false;
        }
        *self = next;
        true
    }
}
