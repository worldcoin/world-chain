use std::{
    mem::MaybeUninit,
    ops::Range,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use alloy_rpc_types_engine::PayloadId;
use flashblocks_primitives::primitives::FlashblocksPayloadV1;
use futures::Stream;
use reth_chain_state::ExecutedBlock;
use reth_node_api::NodePrimitives;
use reth_primitives::RecoveredBlock;
use tokio::sync::watch;
use tracing::{debug, error, warn};

/// A pinned, boxed stream of P2P flashblock payloads.
pub type FlashblocksStream = Pin<Box<dyn Stream<Item = FlashblocksPayloadV1> + Send + 'static>>;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// An executed flashblock with its position in the current payload epoch.
#[derive(Clone, Debug, PartialEq)]
pub struct ExecutedFlashblock<T: NodePrimitives> {
    pub block: ExecutedBlock<T>,
    pub index: u64,
}

impl<T: NodePrimitives> Eq for ExecutedFlashblock<T> {}

impl<T: NodePrimitives> PartialOrd for ExecutedFlashblock<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: NodePrimitives> Ord for ExecutedFlashblock<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.index.cmp(&other.index)
    }
}

/// Minimal pending block state for RPC consumers.
///
/// Only the recovered block and receipts -- no bundle state, no trie data.
#[derive(Clone, Debug)]
pub struct PendingBlock<T: NodePrimitives> {
    pub recovered_block: Arc<RecoveredBlock<T::Block>>,
    pub receipts: Arc<Vec<T::Receipt>>,
}

impl<T: NodePrimitives> PendingBlock<T> {
    pub fn new(recovered_block: Arc<RecoveredBlock<T::Block>>, receipts: Vec<T::Receipt>) -> Self {
        Self {
            recovered_block,
            receipts: Arc::new(receipts),
        }
    }

    pub fn from_executed(fb: &ExecutedFlashblock<T>) -> Self {
        Self {
            recovered_block: Arc::clone(&fb.block.recovered_block),
            receipts: Arc::new(fb.block.execution_output.receipts.clone()),
        }
    }
}

/// Watch channel value -- `Arc` so clone is a refcount bump.
pub type PendingBlockRef<T> = Option<Arc<PendingBlock<T>>>;

// ---------------------------------------------------------------------------
// Cursor -- payload-epoch-aware bitmap cursor
// ---------------------------------------------------------------------------

const MAX_SLOTS: usize = 32;

/// Fixed-capacity cursor over a single payload epoch, identified by
/// [`PayloadId`].
///
/// Tracks which flashblock indices have been received via a `u32` bitmap
/// and detects epoch transitions when the `payload_id` changes. On
/// transition, any missing slots in the old epoch are reported as dropped.
///
/// | Operation          | Implementation               |
/// |--------------------|------------------------------|
/// | `has(i)`           | `bits & (1 << i) != 0`       |
/// | `insert(i)`        | `bits \|= 1 << i`            |
/// | `is_empty()`       | `bits == 0`                  |
/// | `occupied_count()` | `bits.count_ones()`          |
/// | `advance_watermark`| `(bits >> wm).trailing_ones()`|
pub(crate) struct Cursor<T: NodePrimitives> {
    /// Bitmap of received flashblock indices.
    bits: u32,
    /// Lowest un-delivered contiguous index.
    watermark: u64,
    /// Flashblock storage (indexed by flashblock index).
    slots: Vec<MaybeUninit<ExecutedFlashblock<T>>>,
    /// Current epoch's payload id. `None` before the first flashblock.
    payload_id: Option<PayloadId>,
    /// Epoch timestamp from the base payload (for freshness tracking).
    epoch_timestamp: Option<u64>,
}

impl<T: NodePrimitives> Cursor<T> {
    pub(crate) fn new() -> Self {
        let mut slots = Vec::with_capacity(MAX_SLOTS);
        slots.resize_with(MAX_SLOTS, MaybeUninit::uninit);
        Self {
            bits: 0,
            watermark: 0,
            slots,
            payload_id: None,
            epoch_timestamp: None,
        }
    }

    /// Returns the current payload id, if any epoch has started.
    #[inline]
    pub(crate) fn payload_id(&self) -> Option<PayloadId> {
        self.payload_id
    }

    /// Returns the epoch timestamp from the base payload.
    #[inline]
    pub(crate) fn epoch_timestamp(&self) -> Option<u64> {
        self.epoch_timestamp
    }

    /// Check whether `payload_id` matches the current epoch.
    ///
    /// Returns `true` if this is a new epoch (different payload_id).
    #[inline]
    pub(crate) fn is_new_epoch(&self, payload_id: PayloadId) -> bool {
        self.payload_id.is_some_and(|id| id != payload_id)
    }

    /// Report which slots were never received in the current epoch.
    ///
    /// Returns the indices of missing slots between 0 and the highest
    /// received index. Only meaningful when called before resetting.
    pub(crate) fn dropped_slots(&self) -> Vec<u64> {
        if self.bits == 0 {
            return Vec::new();
        }
        let highest = 31 - self.bits.leading_zeros() as u64;
        (0..=highest).filter(|&i| !self.has(i)).collect()
    }

    /// Reset the cursor for a new epoch with the given payload id and
    /// optional timestamp.
    pub(crate) fn start_epoch(&mut self, payload_id: PayloadId, timestamp: Option<u64>) {
        self.bits = 0;
        self.watermark = 0;
        self.slots
            .iter_mut()
            .for_each(|s| *s = MaybeUninit::uninit());
        self.payload_id = Some(payload_id);
        self.epoch_timestamp = timestamp;
    }

    #[inline]
    pub(crate) fn insert(&mut self, fb: ExecutedFlashblock<T>) -> bool {
        let i = fb.index as usize;
        if i >= MAX_SLOTS {
            return false;
        }
        self.bits |= 1u32 << i;
        self.slots[i] = MaybeUninit::new(fb);
        true
    }

    #[inline]
    pub(crate) fn has(&self, idx: u64) -> bool {
        let i = idx as usize;
        i < MAX_SLOTS && self.bits & (1u32 << i) != 0
    }

    #[inline]
    pub(crate) fn get(&self, idx: u64) -> Option<&ExecutedFlashblock<T>> {
        if !self.has(idx) {
            return None;
        }
        // SAFETY: The bitmap guarantees that slot `idx` has been initialised
        // via `insert()` before we reach this point.
        Some(unsafe { self.slots[idx as usize].assume_init_ref() })
    }

    #[inline]
    pub(crate) fn watermark(&self) -> u64 {
        self.watermark
    }

    #[inline]
    pub(crate) fn advance_watermark(&mut self) -> Range<u64> {
        let start = self.watermark;
        let wm = start as u32;
        if wm < MAX_SLOTS as u32 {
            let run = (self.bits >> wm).trailing_ones() as u64;
            self.watermark = start + run;
        }
        start..self.watermark
    }

    #[inline]
    pub(crate) fn occupied_count(&self) -> u32 {
        self.bits.count_ones()
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.bits == 0
    }
}

// ---------------------------------------------------------------------------
// FlashblockExecutor trait
// ---------------------------------------------------------------------------

/// Trait for executing raw flashblock payloads into executed blocks.
///
/// Implementors transform a raw P2P [`FlashblocksPayloadV1`] into an
/// [`ExecutedFlashblock`] by running EVM execution against the current state.
pub trait FlashblockExecutor: Send {
    type Primitives: NodePrimitives;

    /// Execute a raw flashblock payload, returning the executed block and its
    /// index within the current epoch.
    fn execute(
        &mut self,
        payload: FlashblocksPayloadV1,
    ) -> eyre::Result<ExecutedFlashblock<Self::Primitives>>;
}

// ---------------------------------------------------------------------------
// FlashblocksEventStream
// ---------------------------------------------------------------------------

/// Bidirectional event stream at the center of flashblock processing.
///
/// Implements [`Stream`] with a hand-rolled `poll_next`. Epoch boundaries
/// are detected from the flashblock data itself: when a new `payload_id`
/// arrives, the old epoch is finalized, any dropped slots are reported,
/// and the cursor resets.
///
/// ```text
///   ┌─────────────────────────────────────────────────────┐
///   │              FlashblocksEventStream                 │
///   │                                                     │
///   │  ┌───────────────┐       ┌──────────────────────┐   │
///   │  │  P2P stream   │──────▶│  FlashblockExecutor  │   │
///   │  └───────────────┘       │  (validator hook)    │   │
///   │                          └─────────┬────────────┘   │
///   │                                    │                │
///   │                             ┌──────▼──────┐         │
///   │   payload_id change ──────▶ │   Cursor    │         │
///   │   (epoch transition)        │ (bitmap +   │         │
///   │   ├─ log dropped slots      │  payload_id │         │
///   │   └─ reset                  │  + timing)  │         │
///   │                             └──────┬──────┘         │
///   │                                    │ deliver        │
///   │                             ┌──────▼──────┐         │
///   │                             │ watch::Send │─────────┼──▶ FlashblocksState
///   │                             └─────────────┘         │        (Eth API)
///   └─────────────────────────────────────────────────────┘
/// ```
///
/// **Input**: Raw P2P flashblock payloads ([`FlashblocksStream`])
///
/// **Side-effect output**: Pending block via [`watch::Sender`]
///
/// **Yielded items** (`Stream::Item`): Each [`ExecutedFlashblock`]
///
/// **Internal state** (fully owned, no shared mutexes):
/// - [`Cursor`] -- bitmap ordering + payload_id epoch tracking + timing
/// - [`FlashblockExecutor`] -- validator hook that owns execution state
pub struct FlashblocksEventStream<E: FlashblockExecutor> {
    /// Raw P2P flashblock payloads from the network.
    p2p_stream: FlashblocksStream,
    /// Output: pending block for Eth API consumers.
    pending_block: watch::Sender<PendingBlockRef<E::Primitives>>,
    /// Ordering cursor with epoch tracking.
    cursor: Cursor<E::Primitives>,
    /// Validator hook -- transforms raw payloads into executed blocks.
    executor: E,
}

impl<E> FlashblocksEventStream<E>
where
    E: FlashblockExecutor,
{
    /// Create a new event stream.
    pub fn new(
        p2p_stream: FlashblocksStream,
        executor: E,
        pending_block: watch::Sender<PendingBlockRef<E::Primitives>>,
    ) -> Self {
        Self {
            p2p_stream,
            pending_block,
            cursor: Cursor::new(),
            executor,
        }
    }

    /// Convenience: run the stream to completion as a spawnable future.
    ///
    /// Drives `poll_next` until the P2P stream closes, discarding yielded
    /// items (side-effects on the watch channel are the primary output).
    pub async fn run(self)
    where
        E: Unpin,
    {
        use futures::StreamExt;
        let mut this = std::pin::pin!(self);
        while this.next().await.is_some() {}
    }
}

impl<E> Stream for FlashblocksEventStream<E>
where
    E: FlashblockExecutor + Unpin,
{
    type Item = ExecutedFlashblock<E::Primitives>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            match this.p2p_stream.as_mut().poll_next(cx) {
                Poll::Ready(Some(flashblock)) => {
                    let payload_id = flashblock.payload_id;
                    let index = flashblock.index;

                    // ── Epoch transition: new payload_id ────────────
                    if this.cursor.is_new_epoch(payload_id) {
                        let old_id = this.cursor.payload_id().unwrap();
                        let dropped = this.cursor.dropped_slots();

                        if dropped.is_empty() {
                            debug!(
                                old_payload_id = %old_id,
                                new_payload_id = %payload_id,
                                delivered = this.cursor.occupied_count(),
                                "epoch transition, all slots received"
                            );
                        } else {
                            warn!(
                                old_payload_id = %old_id,
                                new_payload_id = %payload_id,
                                delivered = this.cursor.occupied_count(),
                                dropped_count = dropped.len(),
                                dropped_indices = ?dropped,
                                "epoch transition, slots dropped"
                            );
                            metrics::counter!("flashblocks.dropped_slots")
                                .increment(dropped.len() as u64);
                        }

                        metrics::counter!("flashblocks.epoch_transitions").increment(1);

                        // Extract timestamp from the base payload if present.
                        let ts = flashblock.base.as_ref().map(|b| b.timestamp);
                        this.cursor.start_epoch(payload_id, ts);
                        this.pending_block.send_replace(None);
                    } else if this.cursor.payload_id().is_none() {
                        // First flashblock ever — initialize the epoch.
                        let ts = flashblock.base.as_ref().map(|b| b.timestamp);
                        this.cursor.start_epoch(payload_id, ts);
                    }

                    // ── Execute ─────────────────────────────────────
                    match this.executor.execute(flashblock) {
                        Ok(executed) => {
                            if !this.cursor.insert(executed.clone()) {
                                debug!(
                                    %payload_id,
                                    %index,
                                    "flashblock index exceeds cursor capacity"
                                );
                                continue;
                            }

                            deliver(&mut this.cursor, &this.pending_block);
                            return Poll::Ready(Some(executed));
                        }
                        Err(e) => {
                            error!(
                                %payload_id,
                                %index,
                                error = %e,
                                "error executing flashblock"
                            );
                            continue;
                        }
                    }
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Advance the watermark over contiguous slots and publish the latest head
/// to the watch channel.
fn deliver<T: NodePrimitives>(
    cursor: &mut Cursor<T>,
    pending_block: &watch::Sender<PendingBlockRef<T>>,
) {
    let range = cursor.advance_watermark();
    if range.is_empty() {
        return;
    }

    let head_idx = cursor.watermark() - 1;
    if let Some(fb) = cursor.get(head_idx) {
        let block = PendingBlock::from_executed(fb);
        pending_block.send_replace(Some(Arc::new(block)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use reth_chain_state::ExecutedBlock;
    use reth_optimism_primitives::OpPrimitives;
    use tokio::sync::watch;

    fn make_fb(index: u64) -> ExecutedFlashblock<OpPrimitives> {
        ExecutedFlashblock {
            block: ExecutedBlock::default(),
            index,
        }
    }

    struct TestExecutor;

    impl FlashblockExecutor for TestExecutor {
        type Primitives = OpPrimitives;

        fn execute(
            &mut self,
            payload: FlashblocksPayloadV1,
        ) -> eyre::Result<ExecutedFlashblock<OpPrimitives>> {
            Ok(ExecutedFlashblock {
                block: ExecutedBlock::default(),
                index: payload.index,
            })
        }
    }

    fn pid(id: u8) -> PayloadId {
        PayloadId::new([id; 8])
    }

    fn make_payload(payload_id: PayloadId, index: u64) -> FlashblocksPayloadV1 {
        FlashblocksPayloadV1 {
            payload_id,
            index,
            ..Default::default()
        }
    }

    fn make_payload_with_base(
        payload_id: PayloadId,
        index: u64,
        timestamp: u64,
    ) -> FlashblocksPayloadV1 {
        use flashblocks_primitives::primitives::ExecutionPayloadBaseV1;
        FlashblocksPayloadV1 {
            payload_id,
            index,
            base: Some(ExecutionPayloadBaseV1 {
                timestamp,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn make_stream_and_tx() -> (
        tokio::sync::mpsc::Sender<FlashblocksPayloadV1>,
        FlashblocksEventStream<TestExecutor>,
        watch::Receiver<PendingBlockRef<OpPrimitives>>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let p2p = tokio_stream::wrappers::ReceiverStream::new(rx);
        let (watch_tx, watch_rx) = watch::channel(None);
        let stream = FlashblocksEventStream::new(Box::pin(p2p), TestExecutor, watch_tx);
        (tx, stream, watch_rx)
    }

    // -- Cursor --

    #[test]
    fn cursor_insert_and_get() {
        let mut c = Cursor::<OpPrimitives>::new();
        assert!(c.is_empty());
        assert!(c.payload_id().is_none());
        c.start_epoch(pid(1), Some(1000));
        assert_eq!(c.payload_id(), Some(pid(1)));
        assert_eq!(c.epoch_timestamp(), Some(1000));

        assert!(c.insert(make_fb(0)));
        assert!(c.has(0));
        assert!(!c.has(1));
        assert_eq!(c.get(0).unwrap().index, 0);
    }

    #[test]
    fn cursor_epoch_transition_detects_new_id() {
        let mut c = Cursor::<OpPrimitives>::new();
        assert!(!c.is_new_epoch(pid(1))); // no epoch yet

        c.start_epoch(pid(1), None);
        assert!(!c.is_new_epoch(pid(1))); // same id
        assert!(c.is_new_epoch(pid(2))); // different id
    }

    #[test]
    fn cursor_dropped_slots() {
        let mut c = Cursor::<OpPrimitives>::new();
        c.start_epoch(pid(1), None);

        // Receive indices 0, 2, 4 — missing 1 and 3
        c.insert(make_fb(0));
        c.insert(make_fb(2));
        c.insert(make_fb(4));

        let dropped = c.dropped_slots();
        assert_eq!(dropped, vec![1, 3]);
    }

    #[test]
    fn cursor_dropped_slots_none_missing() {
        let mut c = Cursor::<OpPrimitives>::new();
        c.start_epoch(pid(1), None);

        c.insert(make_fb(0));
        c.insert(make_fb(1));
        c.insert(make_fb(2));

        assert!(c.dropped_slots().is_empty());
    }

    #[test]
    fn cursor_dropped_slots_empty() {
        let c = Cursor::<OpPrimitives>::new();
        assert!(c.dropped_slots().is_empty());
    }

    #[test]
    fn cursor_out_of_order() {
        let mut c = Cursor::<OpPrimitives>::new();
        assert!(c.insert(make_fb(2)));
        assert!(!c.has(0));
        assert!(c.has(2));
        assert!(c.insert(make_fb(0)));
        assert_eq!(c.occupied_count(), 2);
    }

    #[test]
    fn cursor_rejects_over_capacity() {
        let mut c = Cursor::<OpPrimitives>::new();
        assert!(!c.insert(make_fb(MAX_SLOTS as u64)));
    }

    #[test]
    fn cursor_watermark_advance() {
        let mut c = Cursor::<OpPrimitives>::new();
        c.insert(make_fb(0));
        c.insert(make_fb(1));
        c.insert(make_fb(3));

        let delivered = c.advance_watermark();
        assert_eq!(delivered, 0..2);
        assert_eq!(c.watermark(), 2);

        c.insert(make_fb(2));
        let delivered = c.advance_watermark();
        assert_eq!(delivered, 2..4);
    }

    #[test]
    fn cursor_bitmap_correctness() {
        let mut c = Cursor::<OpPrimitives>::new();
        c.insert(make_fb(1));
        c.insert(make_fb(3));
        c.insert(make_fb(5));
        assert_eq!(c.bits, 0b00101010);
        assert_eq!(c.occupied_count(), 3);
        assert!(c.advance_watermark().is_empty());

        c.insert(make_fb(0));
        assert_eq!(c.advance_watermark(), 0..2);

        c.insert(make_fb(2));
        assert_eq!(c.advance_watermark(), 2..4);
    }

    #[test]
    fn cursor_start_epoch_resets() {
        let mut c = Cursor::<OpPrimitives>::new();
        c.start_epoch(pid(1), Some(1000));
        c.insert(make_fb(0));
        c.insert(make_fb(1));
        c.advance_watermark();
        assert_eq!(c.watermark(), 2);

        c.start_epoch(pid(2), Some(2000));
        assert!(c.is_empty());
        assert_eq!(c.watermark(), 0);
        assert_eq!(c.payload_id(), Some(pid(2)));
        assert_eq!(c.epoch_timestamp(), Some(2000));
    }

    // -- FlashblocksEventStream --

    #[tokio::test]
    async fn contiguous_delivery() {
        let (tx, stream, rx) = make_stream_and_tx();
        let handle = tokio::spawn(stream.run());

        for i in 0..3 {
            tx.send(make_payload(pid(1), i)).await.unwrap();
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(rx.borrow().is_some());

        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn gap_blocks_delivery() {
        let (tx, stream, rx) = make_stream_and_tx();
        let handle = tokio::spawn(stream.run());

        tx.send(make_payload(pid(1), 0)).await.unwrap();
        tx.send(make_payload(pid(1), 2)).await.unwrap();
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(rx.borrow().is_some());

        tx.send(make_payload(pid(1), 1)).await.unwrap();
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(rx.borrow().is_some());

        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn new_payload_id_resets_epoch() {
        let (tx, stream, rx) = make_stream_and_tx();
        let handle = tokio::spawn(stream.run());

        // Epoch 1
        tx.send(make_payload(pid(1), 0)).await.unwrap();
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(rx.borrow().is_some());

        // Epoch 2 — different payload_id resets cursor and pending
        tx.send(make_payload_with_base(pid(2), 0, 2000))
            .await
            .unwrap();
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        // Pending is re-set by the new epoch's first flashblock
        assert!(rx.borrow().is_some());

        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn same_payload_id_no_reset() {
        let (tx, stream, rx) = make_stream_and_tx();
        let handle = tokio::spawn(stream.run());

        tx.send(make_payload(pid(1), 0)).await.unwrap();
        tx.send(make_payload(pid(1), 1)).await.unwrap();
        tx.send(make_payload(pid(1), 2)).await.unwrap();
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(rx.borrow().is_some());

        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn stream_yields_executed_flashblocks() {
        let (tx, stream, _rx) = make_stream_and_tx();
        let mut pinned = std::pin::pin!(stream);

        tx.send(make_payload(pid(1), 0)).await.unwrap();
        tx.send(make_payload(pid(1), 1)).await.unwrap();

        let fb0 = pinned.next().await.expect("should yield flashblock 0");
        assert_eq!(fb0.index, 0);

        let fb1 = pinned.next().await.expect("should yield flashblock 1");
        assert_eq!(fb1.index, 1);

        drop(tx);
        assert!(pinned.next().await.is_none(), "stream should terminate");
    }

    #[tokio::test]
    async fn terminates_on_stream_close() {
        let (tx, stream, _rx) = make_stream_and_tx();
        let handle = tokio::spawn(stream.run());
        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn full_epoch_delivery() {
        let (tx, stream, rx) = make_stream_and_tx();
        let handle = tokio::spawn(stream.run());

        for i in 0..MAX_SLOTS as u64 {
            tx.send(make_payload(pid(1), i)).await.unwrap();
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(rx.borrow().is_some());

        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn reverse_order_delivers_all() {
        let (tx, stream, rx) = make_stream_and_tx();
        let handle = tokio::spawn(stream.run());

        for i in (0..5).rev() {
            tx.send(make_payload(pid(1), i)).await.unwrap();
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        assert!(
            rx.borrow().is_some(),
            "expected pending head after contiguous delivery"
        );

        drop(tx);
        handle.await.unwrap();
    }
}
