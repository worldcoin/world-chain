use std::{
    future::Future,
    mem::MaybeUninit,
    ops::Range,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use flashblocks_primitives::primitives::FlashblocksPayloadV1;
use futures::Stream;
use reth_chain_state::ExecutedBlock;
use reth_node_api::{Events, NodePrimitives, NodeTypes, PayloadTypes};
use reth_primitives::RecoveredBlock;
use tokio::sync::watch;
use tokio_stream::wrappers::BroadcastStream;
use tracing::debug;

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

/// Events on the flashblock broadcast channel.
#[derive(Clone, Debug)]
pub enum FlashblockEvent<N: NodeTypes> {
    Attributes(<N::Payload as PayloadTypes>::PayloadBuilderAttributes),
    ExecutedFlashblock(ExecutedFlashblock<N::Primitives>),
}

/// Watch channel value -- `Arc` so clone is a refcount bump.
pub type PendingBlockRef<T> = Option<Arc<PendingBlock<T>>>;

// ---------------------------------------------------------------------------
// Cursor -- u32 bitmap + Vec storage
// ---------------------------------------------------------------------------

const MAX_SLOTS: usize = 32;

/// Fixed-capacity cursor over a single payload epoch.
///
/// Pure bitmap math on a `u32`:
///
/// | Operation          | Implementation               |
/// |--------------------|------------------------------|
/// | `has(i)`           | `bits & (1 << i) != 0`       |
/// | `insert(i)`        | `bits \|= 1 << i`            |
/// | `is_empty()`       | `bits == 0`                  |
/// | `occupied_count()` | `bits.count_ones()`          |
/// | `advance_watermark`| `(bits >> wm).trailing_ones()`|
pub(crate) struct Cursor<T: NodePrimitives> {
    bits: u32,
    watermark: u64,
    slots: Vec<MaybeUninit<ExecutedFlashblock<T>>>,
}

impl<T: NodePrimitives> Cursor<T> {
    pub(crate) fn new() -> Self {
        let mut slots = Vec::with_capacity(MAX_SLOTS);
        slots.resize_with(MAX_SLOTS, MaybeUninit::uninit);
        Self {
            bits: 0,
            watermark: 0,
            slots,
        }
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
// FlashblocksEventStream
// ---------------------------------------------------------------------------

/// Async driver that merges two broadcast streams into a single ordered event
/// loop.
///
/// Flashblocks are buffered in a [`Cursor`] and delivered in contiguous index
/// order. The latest head is published via a [`watch::Sender`] holding
/// `Arc<PendingBlock>` -- readers clone the `Arc` (refcount bump, zero
/// allocation).
///
/// Attribute events from **either** stream reset the cursor and clear the
/// pending head.
///
/// This is a `Future` (driver), not a `Stream` -- it runs to completion when
/// either broadcast channel closes.
#[pin_project::pin_project]
pub struct FlashblocksEventStream<T: NodeTypes> {
    #[pin]
    peers_stream: BroadcastStream<FlashblockEvent<T>>,

    #[pin]
    payloads_stream: BroadcastStream<Events<T::Payload>>,

    pending_block: watch::Sender<PendingBlockRef<T::Primitives>>,

    cursor: Cursor<T::Primitives>,
}

impl<T> FlashblocksEventStream<T>
where
    T: NodeTypes,
{
    pub fn new(
        peers_stream: BroadcastStream<FlashblockEvent<T>>,
        payloads_stream: BroadcastStream<Events<T::Payload>>,
        pending_block: watch::Sender<PendingBlockRef<T::Primitives>>,
    ) -> Self {
        Self {
            peers_stream,
            payloads_stream,
            pending_block,
            cursor: Cursor::new(),
        }
    }

    fn reset_epoch(
        cursor: &mut Cursor<T::Primitives>,
        pending_block: &watch::Sender<PendingBlockRef<T::Primitives>>,
    ) {
        if !cursor.is_empty() {
            debug!(
                flushed = cursor.occupied_count(),
                "new epoch, clearing pending"
            );
        }
        *cursor = Cursor::new();
        pending_block.send_replace(None);
    }

    /// Advance the watermark over contiguous slots and publish the latest head
    /// to the watch channel.
    fn deliver(
        cursor: &mut Cursor<T::Primitives>,
        pending_block: &watch::Sender<PendingBlockRef<T::Primitives>>,
    ) {
        let range = cursor.advance_watermark();
        if range.is_empty() {
            return;
        }

        // The highest contiguous slot is always `watermark - 1`.
        let head_idx = cursor.watermark() - 1;
        if let Some(fb) = cursor.get(head_idx) {
            let block = PendingBlock::from_executed(fb);
            pending_block.send_replace(Some(Arc::new(block)));
        }
    }
}

impl<T> Future for FlashblocksEventStream<T>
where
    T: NodeTypes,
    T::Primitives: Unpin,
    <T::Payload as PayloadTypes>::BuiltPayload: Unpin,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();

        let mut inserted_any = false;
        let mut flushed = false;

        loop {
            match this.peers_stream.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(FlashblockEvent::ExecutedFlashblock(fb)))) => {
                    if !this.cursor.insert(fb) {
                        debug!("flashblock index exceeds cursor capacity");
                    } else {
                        inserted_any = true;
                    }
                }
                Poll::Ready(Some(Ok(FlashblockEvent::Attributes(_)))) => {
                    flushed = true;
                    Self::reset_epoch(this.cursor, this.pending_block);
                }
                Poll::Ready(Some(Err(e))) => {
                    debug!(error = %e, "peers broadcast lagging");
                    continue;
                }
                Poll::Ready(None) => return Poll::Ready(()),
                Poll::Pending => break,
            }
        }

        loop {
            match this.payloads_stream.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(Events::BuiltPayload(_)))) => {}
                Poll::Ready(Some(Ok(Events::Attributes(_)))) => {
                    flushed = true;
                    Self::reset_epoch(this.cursor, this.pending_block);
                }
                Poll::Ready(Some(Err(e))) => {
                    debug!(error = %e, "payloads broadcast lagging");
                    continue;
                }
                Poll::Ready(None) => return Poll::Ready(()),
                Poll::Pending => break,
            }
        }

        if inserted_any && !flushed {
            Self::deliver(this.cursor, this.pending_block);
        }

        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reth_chain_state::ExecutedBlock;
    use reth_optimism_node::OpEngineTypes;
    use reth_optimism_primitives::OpPrimitives;
    use tokio::sync::{broadcast, watch};

    type TestEvent = FlashblockEvent<reth_optimism_node::OpNode>;
    type TestPayloadEvent = Events<OpEngineTypes>;

    fn make_fb(index: u64) -> ExecutedFlashblock<OpPrimitives> {
        ExecutedFlashblock {
            block: ExecutedBlock::default(),
            index,
        }
    }

    fn channels() -> (
        broadcast::Sender<TestEvent>,
        broadcast::Sender<TestPayloadEvent>,
        BroadcastStream<TestEvent>,
        BroadcastStream<TestPayloadEvent>,
    ) {
        let (fb_tx, fb_rx) = broadcast::channel(64);
        let (pe_tx, pe_rx) = broadcast::channel(64);
        (
            fb_tx,
            pe_tx,
            BroadcastStream::new(fb_rx),
            BroadcastStream::new(pe_rx),
        )
    }

    fn poll_once(stream: &mut FlashblocksEventStream<reth_optimism_node::OpNode>) {
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        let _ = Pin::new(stream).poll(&mut cx);
    }

    fn make_stream(
        fb_rx: BroadcastStream<TestEvent>,
        pe_rx: BroadcastStream<TestPayloadEvent>,
    ) -> (
        FlashblocksEventStream<reth_optimism_node::OpNode>,
        watch::Receiver<PendingBlockRef<OpPrimitives>>,
    ) {
        let (tx, rx) = watch::channel(None);
        (FlashblocksEventStream::new(fb_rx, pe_rx, tx), rx)
    }

    // -- Cursor --

    #[test]
    fn cursor_insert_and_get() {
        let mut c = Cursor::<OpPrimitives>::new();
        assert!(c.is_empty());
        assert!(c.insert(make_fb(0)));
        assert!(c.has(0));
        assert!(!c.has(1));
        assert_eq!(c.get(0).unwrap().index, 0);
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
    fn cursor_drop_cleans_up() {
        let mut c = Cursor::<OpPrimitives>::new();
        for i in 0..10 {
            c.insert(make_fb(i));
        }
        assert_eq!(c.occupied_count(), 10);
        drop(c);
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

    // -- FlashblocksEventStream --

    #[tokio::test]
    async fn contiguous_delivery() {
        let (fb_tx, _pe_tx, fb_rx, pe_rx) = channels();
        let (mut st, rx) = make_stream(fb_rx, pe_rx);

        for i in 0..3 {
            fb_tx
                .send(FlashblockEvent::ExecutedFlashblock(make_fb(i)))
                .unwrap();
        }
        tokio::task::yield_now().await;
        poll_once(&mut st);
        assert!(rx.borrow().is_some());
    }

    #[tokio::test]
    async fn gap_blocks_delivery() {
        let (fb_tx, _pe_tx, fb_rx, pe_rx) = channels();
        let (mut st, rx) = make_stream(fb_rx, pe_rx);

        fb_tx
            .send(FlashblockEvent::ExecutedFlashblock(make_fb(0)))
            .unwrap();
        fb_tx
            .send(FlashblockEvent::ExecutedFlashblock(make_fb(2)))
            .unwrap();
        tokio::task::yield_now().await;
        poll_once(&mut st);
        assert!(rx.borrow().is_some());

        fb_tx
            .send(FlashblockEvent::ExecutedFlashblock(make_fb(1)))
            .unwrap();
        tokio::task::yield_now().await;
        poll_once(&mut st);
        assert!(rx.borrow().is_some());
    }

    #[tokio::test]
    async fn attributes_clears_pending() {
        let (fb_tx, _pe_tx, fb_rx, pe_rx) = channels();
        let (mut st, rx) = make_stream(fb_rx, pe_rx);

        fb_tx
            .send(FlashblockEvent::ExecutedFlashblock(make_fb(0)))
            .unwrap();
        tokio::task::yield_now().await;
        poll_once(&mut st);
        assert!(rx.borrow().is_some());

        fb_tx
            .send(FlashblockEvent::Attributes(Default::default()))
            .unwrap();
        tokio::task::yield_now().await;
        poll_once(&mut st);
        assert!(rx.borrow().is_none());

        fb_tx
            .send(FlashblockEvent::ExecutedFlashblock(make_fb(0)))
            .unwrap();
        tokio::task::yield_now().await;
        poll_once(&mut st);
        assert!(rx.borrow().is_some());
    }

    #[tokio::test]
    async fn payload_attributes_clears_pending() {
        let (fb_tx, pe_tx, fb_rx, pe_rx) = channels();
        let (mut st, rx) = make_stream(fb_rx, pe_rx);

        fb_tx
            .send(FlashblockEvent::ExecutedFlashblock(make_fb(0)))
            .unwrap();
        tokio::task::yield_now().await;
        poll_once(&mut st);
        assert!(rx.borrow().is_some());

        pe_tx.send(Events::Attributes(Default::default())).unwrap();
        tokio::task::yield_now().await;
        poll_once(&mut st);
        assert!(rx.borrow().is_none());
    }

    #[tokio::test]
    async fn terminates_on_peers_close() {
        let (fb_tx, _pe_tx, fb_rx, pe_rx) = channels();
        let (mut st, _rx) = make_stream(fb_rx, pe_rx);
        drop(fb_tx);
        tokio::task::yield_now().await;
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(Pin::new(&mut st).poll(&mut cx).is_ready());
    }

    #[tokio::test]
    async fn terminates_on_payloads_close() {
        let (_fb_tx, pe_tx, fb_rx, pe_rx) = channels();
        let (mut st, _rx) = make_stream(fb_rx, pe_rx);
        drop(pe_tx);
        tokio::task::yield_now().await;
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(Pin::new(&mut st).poll(&mut cx).is_ready());
    }

    #[tokio::test]
    async fn full_epoch_delivery() {
        let (fb_tx, _pe_tx, fb_rx, pe_rx) = channels();
        let (mut st, rx) = make_stream(fb_rx, pe_rx);

        for i in 0..MAX_SLOTS as u64 {
            fb_tx
                .send(FlashblockEvent::ExecutedFlashblock(make_fb(i)))
                .unwrap();
        }
        tokio::task::yield_now().await;
        poll_once(&mut st);
        assert!(rx.borrow().is_some());
    }

    #[tokio::test]
    async fn reverse_order_delivers_all() {
        let (fb_tx, _pe_tx, fb_rx, pe_rx) = channels();
        let (mut st, rx) = make_stream(fb_rx, pe_rx);

        for i in (0..5).rev() {
            fb_tx
                .send(FlashblockEvent::ExecutedFlashblock(make_fb(i)))
                .unwrap();
        }
        tokio::task::yield_now().await;
        poll_once(&mut st);

        // All 5 slots are contiguous (0..5), so the watch should hold the
        // head at index 4.
        let head = rx.borrow();
        assert!(
            head.is_some(),
            "expected pending head after contiguous delivery"
        );
    }
}
