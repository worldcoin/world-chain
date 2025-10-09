// A processor for that performs parallel state validation of incoming
// Flashblocks using the Flashblock Access List (FBAL).

use std::future::Future;

use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use flashblocks_primitives::primitives::FlashblocksPayloadV1;
use futures::stream::StreamExt;
use futures::Stream;
use reth::{
    api::{Events, PayloadTypes},
    providers::CanonStateNotification,
};
use reth_optimism_node::OpBuiltPayload;
use reth_optimism_primitives::OpPrimitives;
use tokio::sync::broadcast;

pub struct FlashblockBalProcessor<T: PayloadTypes, St, P> {
    /// Handle to the Flashblocks stream.
    p2p_handle: FlashblocksHandle,
    /// Sender to the [`EngineTree`] for updating the pending block.
    payload_events: broadcast::Sender<Events<T>>,
    /// Chain event subscriptions.
    chain_events: St,
    /// Inner state
    inner: FlashblockBalStateValidator<P>,
}

impl<T: PayloadTypes, St, P> FlashblockBalProcessor<T, St, P> {
    /// Create a new [`FlashblockBalProcessor`].
    pub fn new(
        p2p_handle: FlashblocksHandle,
        payload_events: broadcast::Sender<Events<T>>,
        chain_events: St,
        provider: P,
    ) -> Self {
        Self {
            p2p_handle,
            payload_events,
            chain_events,
            inner: FlashblockBalStateValidator::new(provider),
        }
    }

    pub fn spawn(self) -> impl Future<Output = ()>
    where
        St: Stream<Item = CanonStateNotification<OpPrimitives>> + Unpin + Send + 'static,
    {
        async move {
            let mut fb_stream = self.p2p_handle.flashblock_stream();
            let mut chain_events = self.chain_events;

            loop {
                tokio::select! {
                    Some(fb) = fb_stream.next() => {
                        // self.inner.on_new_flashblock(fb);
                    }
                    Some(state) = chain_events.next() => {
                        // self.inner.on_new_state(state);
                    }

                }
            }
        }
    }
}

pub struct FlashblockBalStateValidator<P> {
    pending_block: Option<OpBuiltPayload>,
    flashblocks: Vec<FlashblocksPayloadV1>,
    provider: P,
}

impl<P> FlashblockBalStateValidator<P> {
    pub fn new(provider: P) -> Self {
        Self {
            pending_block: None,
            flashblocks: Vec::new(),
            provider,
        }
    }

    pub fn on_new_state(&mut self, state: CanonStateNotification<OpPrimitives>) {
        let tip = state.tip().header().number;
        // Make sure we are ahead of the cannon state
        if let Some(pending) = &self.pending_block {
            if pending.block().header().number <= tip {
                self.pending_block = None;
                self.flashblocks.clear();
            }
        }
    }

    pub fn on_new_flashblock(&mut self, fb: FlashblocksPayloadV1) {
        // pre checks
        let latest_flashblock = self.flashblocks.pop();
        if let Some(latest) = latest_flashblock {
            if latest.payload_id != fb.payload_id {
                self.clear();
                self.flashblocks.push(fb);
            } else if self.is_valid_extension(&fb) {
                self.flashblocks.push(latest);
            } else {
                return;
            }
        }

        // fetch the state provider for the parent
        // let parent = 
        // self.process_flashblock(fb);
    }

    fn clear(&mut self) {
        self.pending_block = None;
        self.flashblocks.clear();
    }

    fn validate(&mut self, fb: FlashblocksPayloadV1) {
        // Validate the flashblock against the pending block state using the FBAL
    }

    // todo
    fn is_valid_extension(&self, fb: &FlashblocksPayloadV1) -> bool {
        true
    }

    fn process_flashblock(&mut self, fb: FlashblocksPayloadV1) {
        // Process the flashblock and update the pending block if valid
    }
}
