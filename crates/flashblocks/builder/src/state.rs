//! Shared flashblocks state for RPC consumers.
//!
//! [`FlashblocksState`] wraps a [`watch::Receiver`] that carries the latest
//! pending block produced by the [`FlashblocksEventStream`]. It provides
//! convenient accessors for the Eth API layer.
//!
//! [`FlashblocksEventStream`]: crate::event_stream::FlashblocksEventStream

use std::sync::Arc;

use reth_node_api::NodePrimitives;
use tokio::sync::watch;

use crate::event_stream::{PendingBlock, PendingBlockRef};

/// Read-only handle to the current flashblocks pending block.
///
/// Cheap to clone (just an `Arc` + watch receiver).
#[derive(Clone, Debug)]
pub struct FlashblocksState<T: NodePrimitives> {
    pending: watch::Receiver<PendingBlockRef<T>>,
}

impl<T: NodePrimitives> FlashblocksState<T> {
    /// Create a new state handle from a watch receiver.
    pub fn new(pending: watch::Receiver<PendingBlockRef<T>>) -> Self {
        Self { pending }
    }

    /// Returns the current pending block, if any.
    pub fn pending_block(&self) -> Option<Arc<PendingBlock<T>>> {
        self.pending.borrow().clone()
    }

    /// Returns a clone of the underlying watch receiver.
    ///
    /// Useful for handing to RPC builders that need their own receiver.
    pub fn receiver(&self) -> watch::Receiver<PendingBlockRef<T>> {
        self.pending.clone()
    }

    /// Wait for the next pending block update.
    ///
    /// Returns `Err` if the sender half is dropped.
    pub async fn changed(&mut self) -> Result<(), watch::error::RecvError> {
        self.pending.changed().await
    }
}
