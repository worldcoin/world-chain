//! Canon-aware flashblock event stream.
//!
//! Merges a raw flashblock stream with canonical chain notifications, yielding
//! [`ChainEvent::Pending`] only when the flashblock's epoch parent matches
//! the current canonical tip, and [`ChainEvent::Canon`] whenever the tip
//! changes.

use flashblocks_primitives::primitives::FlashblocksPayloadV1;
use futures::{
    Stream, StreamExt,
    stream::{self, PollNext},
};
use reth::{payload::PayloadId, rpc::types::BlockNumHash};
use std::{
    collections::VecDeque,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

#[derive(Clone, Debug)]
pub enum ChainEvent {
    /// A new canonical tip has been observed. Consumers should clear any
    /// pending flashblocks that are stale relative to the new tip.
    Canon(BlockNumHash),
    /// A flashblock has been received whose epoch parent matches the current
    /// canonical tip. Zero-copy via [`Arc`] — no payload cloning through the
    /// buffer or downstream consumers.
    Pending(Arc<FlashblocksPayloadV1>),
}

/// Events yielded by [`WorldChainEventsStream`].
#[derive(Clone, Debug)]
pub enum WorldChainEvent<T> {
    /// An event emitted when executable pending flashblocks are observed.
    Chain(ChainEvent),
    /// An event emitted by any source.
    Event(T),
}

/// A stream of [`WorldChainEvent`]s that merges flashblocks with canonical
/// chain notifications, reducing them through a [`BufferedFlashblocks`]
/// state machine.
///
/// A [`ChainEvent::Pending`] is emitted only when the flashblock's epoch parent
/// matches the canonical tip. Stale flashblocks are silently discarded.
/// Flashblocks are buffered when the epoch parent is not yet canonical, but the
/// [`PayloadId`] is fresh. A [`ChainEvent::Canon`] is emitted on every
/// canonical tip change so consumers can clear pending state.
pub type WorldChainEventsStream<T> = Pin<Box<dyn Stream<Item = WorldChainEvent<T>> + Send>>;

/// Constructs a [`WorldChainEventsStream`] by merging a flashblock stream with
/// canonical chain notifications, reducing through [`BufferedFlashblocks`], and
/// applying `hook` to each yielded event.
#[must_use]
pub fn world_chain_events_stream<T, F>(
    flashblocks: Pin<Box<dyn Stream<Item = ChainEvent> + Send>>,
    canon: Pin<Box<dyn Stream<Item = ChainEvent> + Send>>,
    mut hook: F,
) -> WorldChainEventsStream<T>
where
    T: Send + Unpin + 'static,
    F: FnMut(&WorldChainEvent<T>) -> Option<WorldChainEvent<T>> + Send + 'static,
{
    let merged =
        futures::stream::select_with_strategy(flashblocks, canon, |_: &mut ()| PollNext::Left);

    BufferedStream::new(merged)
        .map(WorldChainEvent::Chain)
        .flat_map(move |event| {
            let extra = hook(&event);
            stream::iter(std::iter::once(event).chain(extra))
        })
        .boxed()
}

// ---------------------------------------------------------------------------
// BufferedStream — zero-allocation stream adapter
// ---------------------------------------------------------------------------

/// Stream adapter that wraps a merged `ChainEvent` stream and map reduces it
/// into a [`BufferedFlashblocks`].
#[pin_project::pin_project]
#[doc = ""]
struct BufferedStream<S> {
    #[pin]
    inner: S,
    state: BufferedFlashblocks,
}

impl<S> BufferedStream<S> {
    fn new(inner: S) -> Self {
        Self {
            inner,
            state: BufferedFlashblocks::default(),
        }
    }
}

impl<S: Stream<Item = ChainEvent>> Stream for BufferedStream<S> {
    type Item = ChainEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        // Drain buffered output first.
        if let Some(event) = this.state.output.pop_front() {
            return Poll::Ready(Some(event));
        }

        // Poll inner stream, reduce through state machine, yield first output.
        match this.inner.poll_next(cx) {
            Poll::Ready(Some(event)) => {
                this.state.step(event);
                Poll::Ready(this.state.output.pop_front())
            }
            // Inner exhausted — drain any remaining buffered output before closing.
            Poll::Ready(None) => Poll::Ready(this.state.output.pop_front()),
            Poll::Pending => Poll::Pending,
        }
    }
}

// ---------------------------------------------------------------------------
// Epoch — scoped state for a single flashblock epoch
// ---------------------------------------------------------------------------

/// Maximum number of flashblocks per epoch.
pub(crate) const MAX_FLASHBLOCKS: usize = 12;

/// State for a single flashblock epoch: the parent block it builds on,
/// its payload identifier, and a fixed-size buffer of received flashblocks.
pub(crate) struct BlockEpochState {
    /// The parent block this epoch builds on.
    parent: BlockNumHash,
    /// Payload identifier for this epoch.
    payload_id: PayloadId,
    /// Drain watermark — next index to yield.
    cursor: usize,
    /// Fixed-size sparse buffer indexed by flashblock sequence number.
    /// 96 bytes inline — no heap allocation.
    buffer: [Option<Arc<FlashblocksPayloadV1>>; MAX_FLASHBLOCKS],
}

impl BlockEpochState {
    /// Create a new epoch from a base flashblock. Returns `None` if the base
    /// is stale (parent behind the canonical tip) or missing its base field.
    fn try_new(fb: Arc<FlashblocksPayloadV1>, canon_tip: Option<BlockNumHash>) -> Option<Self> {
        let base = fb.base.as_ref()?;

        // Stale check: reject if the epoch's parent is behind the canon tip.
        let parent_number = base.block_number.saturating_sub(1);
        if canon_tip.is_some_and(|tip| parent_number < tip.number) {
            return None;
        }

        let mut epoch = Self {
            parent: BlockNumHash {
                number: parent_number,
                hash: base.parent_hash,
            },
            payload_id: fb.payload_id,
            cursor: 0,
            buffer: Default::default(),
        };
        epoch.insert(fb);
        Some(epoch)
    }

    /// Insert a flashblock at its sequence index. Returns `false` if the
    /// payload_id doesn't match, the index is out of bounds, or the slot
    /// is already occupied.
    fn insert(&mut self, fb: Arc<FlashblocksPayloadV1>) -> bool {
        let idx = fb.index as usize;
        if fb.payload_id != self.payload_id || idx >= MAX_FLASHBLOCKS || self.buffer[idx].is_some()
        {
            return false;
        }
        self.buffer[idx] = Some(fb);
        true
    }
}

// ---------------------------------------------------------------------------
// BufferedFlashblocks — stateful reducer with Extend + Iterator
// ---------------------------------------------------------------------------

/// Buffers flashblocks for the current epoch, gating output on the canonical
/// tip. Phase is derived from state — not tracked separately:
///
/// - `epoch.is_none()` → no active epoch
/// - `epoch.is_some() && canon_tip != epoch.parent` → pending (buffering)
/// - `epoch.is_some() && canon_tip == epoch.parent` → executable (draining)
///
/// Implements [`Extend<ChainEvent>`] to accept input events and
/// [`Iterator<Item = ChainEvent>`] to drain output events.
#[derive(Default)]
pub struct BufferedFlashblocks {
    /// Current epoch, if any. `None` means no active epoch.
    epoch: Option<BlockEpochState>,
    /// Most recent canonical tip.
    canon_tip: Option<BlockNumHash>,
    /// Output events ready to be yielded by the iterator.
    output: VecDeque<ChainEvent>,
}

impl BufferedFlashblocks {
    /// Process a single input event, updating state and buffering output.
    fn step(&mut self, event: ChainEvent) {
        match event {
            ChainEvent::Canon(tip) => {
                self.canon_tip = Some(tip);
                self.output.push_back(ChainEvent::Canon(tip));
            }
            ChainEvent::Pending(ref fb) if fb.base.is_some() => {
                self.epoch = BlockEpochState::try_new(Arc::clone(fb), self.canon_tip);
            }
            ChainEvent::Pending(fb) => {
                if let Some(epoch) = &mut self.epoch {
                    epoch.insert(fb);
                }
            }
        }
        self.drain();
    }

    /// Drain contiguous flashblocks from the cursor into the output queue,
    /// but only if the epoch is anchored to the canonical tip.
    fn drain(&mut self) {
        let canon_tip = self.canon_tip;
        let Some(ref mut epoch) = self.epoch else {
            return;
        };
        if !canon_tip.is_some_and(|tip| tip == epoch.parent) {
            return;
        }
        while let Some(Some(_)) = epoch.buffer.get(epoch.cursor) {
            let fb = epoch.buffer[epoch.cursor].take().unwrap();
            epoch.cursor += 1;
            self.output.push_back(ChainEvent::Pending(fb));
        }
    }
}

impl Iterator for BufferedFlashblocks {
    type Item = ChainEvent;

    fn next(&mut self) -> Option<Self::Item> {
        self.output.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use flashblocks_primitives::primitives::{
        ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1,
    };

    fn canon(number: u64, hash: B256) -> ChainEvent {
        ChainEvent::Canon(BlockNumHash { number, hash })
    }

    fn base_fb(
        payload_id: PayloadId,
        index: u64,
        parent_hash: B256,
        block_number: u64,
    ) -> ChainEvent {
        ChainEvent::Pending(Arc::new(FlashblocksPayloadV1 {
            payload_id,
            index,
            base: Some(ExecutionPayloadBaseV1 {
                parent_hash,
                block_number,
                timestamp: block_number + 1000, // well above any block number
                ..Default::default()
            }),
            diff: ExecutionPayloadFlashblockDeltaV1::default(),
            metadata: Default::default(),
        }))
    }

    fn delta_fb(payload_id: PayloadId, index: u64) -> ChainEvent {
        ChainEvent::Pending(Arc::new(FlashblocksPayloadV1 {
            payload_id,
            index,
            base: None,
            diff: ExecutionPayloadFlashblockDeltaV1::default(),
            metadata: Default::default(),
        }))
    }

    fn pid(b: u8) -> PayloadId {
        PayloadId::new([b; 8])
    }

    fn hash(b: u8) -> B256 {
        B256::with_last_byte(b)
    }

    fn collect_pending(buf: &mut BufferedFlashblocks) -> Vec<u64> {
        buf.by_ref()
            .filter_map(|e| match e {
                ChainEvent::Pending(fb) => Some(fb.index),
                _ => None,
            })
            .collect()
    }

    fn collect_all(buf: &mut BufferedFlashblocks) -> Vec<ChainEvent> {
        buf.by_ref().collect()
    }

    // -----------------------------------------------------------------------
    // Core state machine tests
    // -----------------------------------------------------------------------

    #[test]
    fn no_output_before_canon_tip() {
        let mut buf = BufferedFlashblocks::default();

        // Send a base flashblock — no canon tip yet, goes to Pending
        buf.step(base_fb(pid(1), 0, hash(0), 1));
        assert!(
            collect_pending(&mut buf).is_empty(),
            "should not yield without canon tip"
        );
    }

    #[test]
    fn canon_tip_triggers_drain() {
        let mut buf = BufferedFlashblocks::default();

        // Canon tip first, then base flashblock whose parent matches
        buf.step(canon(0, hash(0)));
        let events = collect_all(&mut buf);
        assert_eq!(events.len(), 1); // just the canon event
        assert!(matches!(events[0], ChainEvent::Canon(_)));

        // Now a base flashblock building on block 1 with parent hash(0)
        buf.step(base_fb(pid(1), 0, hash(0), 1));
        let indices = collect_pending(&mut buf);
        assert_eq!(
            indices,
            vec![0],
            "should drain immediately when parent matches canon tip"
        );
    }

    #[test]
    fn canon_tip_after_buffered_flashblock_flushes() {
        let mut buf = BufferedFlashblocks::default();

        // Base flashblock arrives first — parent hash(5), block_number 6
        buf.step(base_fb(pid(1), 0, hash(5), 6));
        assert!(collect_pending(&mut buf).is_empty(), "no canon tip yet");

        // Delta flashblock for same epoch
        buf.step(delta_fb(pid(1), 1));
        assert!(collect_pending(&mut buf).is_empty(), "still no canon tip");

        // Now canon tip arrives matching the parent
        buf.step(canon(5, hash(5)));
        let events: Vec<_> = collect_all(&mut buf);

        // Should yield: Canon(5), Pending(0), Pending(1)
        assert!(matches!(events[0], ChainEvent::Canon(_)));
        assert_eq!(events.len(), 3);

        let indices: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                ChainEvent::Pending(fb) => Some(fb.index),
                _ => None,
            })
            .collect();
        assert_eq!(indices, vec![0, 1]);
    }

    #[test]
    fn out_of_order_flashblocks_buffered_until_contiguous() {
        let mut buf = BufferedFlashblocks::default();

        buf.step(canon(0, hash(0)));
        collect_all(&mut buf); // drain canon

        // Base at index 0
        buf.step(base_fb(pid(1), 0, hash(0), 1));
        assert_eq!(collect_pending(&mut buf), vec![0]);

        // Index 2 arrives before 1 — gap, can't drain
        buf.step(delta_fb(pid(1), 2));
        assert!(collect_pending(&mut buf).is_empty(), "gap at index 1");

        // Index 1 fills the gap — both 1 and 2 should drain
        buf.step(delta_fb(pid(1), 1));
        assert_eq!(collect_pending(&mut buf), vec![1, 2]);
    }

    #[test]
    fn stale_base_flashblock_discarded() {
        let mut buf = BufferedFlashblocks::default();

        // Canon tip is at block 10
        buf.step(canon(10, hash(10)));
        collect_all(&mut buf);

        // Base flashblock building on block 5 (parent_number=4 < tip=10) — stale
        buf.step(base_fb(pid(1), 0, hash(4), 5));
        assert!(
            collect_pending(&mut buf).is_empty(),
            "stale flashblock should be discarded"
        );
    }

    #[test]
    fn new_epoch_resets_buffer() {
        let mut buf = BufferedFlashblocks::default();

        buf.step(canon(0, hash(0)));
        collect_all(&mut buf);

        // Epoch A
        buf.step(base_fb(pid(1), 0, hash(0), 1));
        assert_eq!(collect_pending(&mut buf), vec![0]);
        buf.step(delta_fb(pid(1), 1));
        assert_eq!(collect_pending(&mut buf), vec![1]);

        // Epoch B — new base with different payload_id, same parent
        buf.step(base_fb(pid(2), 0, hash(0), 1));
        let indices = collect_pending(&mut buf);
        assert_eq!(indices, vec![0], "new epoch should reset and yield base");
    }

    #[test]
    fn canon_event_always_yielded() {
        let mut buf = BufferedFlashblocks::default();

        // Multiple canon events should all be yielded
        buf.step(canon(0, hash(0)));
        buf.step(canon(1, hash(1)));
        buf.step(canon(2, hash(2)));

        let events = collect_all(&mut buf);
        let canon_numbers: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                ChainEvent::Canon(tip) => Some(tip.number),
                _ => None,
            })
            .collect();
        assert_eq!(canon_numbers, vec![0, 1, 2]);
    }

    #[test]
    fn non_base_flashblock_ignored_when_uninitialized() {
        let mut buf = BufferedFlashblocks::default();

        buf.step(canon(0, hash(0)));
        collect_all(&mut buf);

        // Delta without any base — should be silently ignored
        buf.step(delta_fb(pid(1), 5));
        assert!(collect_pending(&mut buf).is_empty());
    }

    #[test]
    fn canon_tip_not_matching_parent_does_not_drain() {
        let mut buf = BufferedFlashblocks::default();

        // Base building on hash(5) at block 6
        buf.step(base_fb(pid(1), 0, hash(5), 6));
        assert!(collect_pending(&mut buf).is_empty());

        // Canon tip at block 3, hash(3) — doesn't match parent hash(5)
        buf.step(canon(3, hash(3)));
        let events = collect_all(&mut buf);

        // Canon event is yielded, but no pending drained
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], ChainEvent::Canon(_)));
    }

    #[test]
    fn batch_processes_multiple_events() {
        let mut buf = BufferedFlashblocks::default();
        for e in [
            canon(0, hash(0)),
            base_fb(pid(1), 0, hash(0), 1),
            delta_fb(pid(1), 1),
            delta_fb(pid(1), 2),
        ] {
            buf.step(e);
        }

        let events = collect_all(&mut buf);

        // Canon(0), Pending(0), Pending(1), Pending(2)
        assert_eq!(events.len(), 4);
        assert!(matches!(events[0], ChainEvent::Canon(_)));

        let indices: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                ChainEvent::Pending(fb) => Some(fb.index),
                _ => None,
            })
            .collect();
        assert_eq!(indices, vec![0, 1, 2]);
    }

    #[test]
    fn duplicate_index_ignored() {
        let mut buf = BufferedFlashblocks::default();

        buf.step(canon(0, hash(0)));
        collect_all(&mut buf);

        buf.step(base_fb(pid(1), 0, hash(0), 1));
        assert_eq!(collect_pending(&mut buf), vec![0]);

        // Same index again — ignored
        buf.step(delta_fb(pid(1), 0));
        assert!(collect_pending(&mut buf).is_empty());
    }

    #[test]
    fn wrong_payload_id_ignored() {
        let mut buf = BufferedFlashblocks::default();

        buf.step(canon(0, hash(0)));
        collect_all(&mut buf);

        buf.step(base_fb(pid(1), 0, hash(0), 1));
        collect_pending(&mut buf);

        // Delta with wrong payload_id — ignored
        buf.step(delta_fb(pid(99), 1));
        assert!(collect_pending(&mut buf).is_empty());
    }
}
