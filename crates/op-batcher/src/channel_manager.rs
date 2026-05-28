//! Channel manager: owns the buffered L2 blocks, the single open channel, and
//! the queue of closed channels with frames still to submit.
//!
//! Mirrors `op-batcher/batcher/channel_manager.go` (`channelManager`), pared to
//! the v1 model. Key invariants preserved from upstream:
//! * one open channel at a time (`current`);
//! * frames emitted and submitted strictly in order (Holocene-safe);
//! * blocks pruned only once they become safe.

use std::{collections::VecDeque, sync::Arc};

use alloy_primitives::B256;
use kona_protocol::ChannelId;
use tracing::{debug, warn};

use crate::{
    channel::Channel,
    channel_out::{FrameOut, FullReason},
    metrics::BatcherMetrics,
    source::L2BlockData,
};

/// Error returned when an added block does not extend the manager's tip.
#[derive(Debug, thiserror::Error)]
pub enum ChannelManagerError {
    #[error("reorg detected: block {number} parent {parent} != tip {tip}")]
    Reorg {
        number: u64,
        parent: B256,
        tip: B256,
    },
    #[error("channel encoding error: {0}")]
    Channel(#[from] crate::channel_out::ChannelOutError),
}

/// Parameters the manager needs to build channels. Derived from [`BatcherConfig`].
#[derive(Debug, Clone, Copy)]
pub struct ChannelParams {
    pub max_rlp_bytes: usize,
    pub max_frame_size: usize,
    /// Target compressed output size per channel (`MaxDataSize`).
    pub target_output: usize,
    pub approx_compr_ratio: f64,
    pub max_channel_duration: u64,
}

/// A frame ready to submit, tagged with its channel so confirmations can be
/// routed back.
#[derive(Debug, Clone)]
pub struct PendingFrame {
    pub channel_id: ChannelId,
    pub frame: FrameOut,
    /// L2 block range of the owning channel (for metrics / persistence).
    pub l2_range: Option<(u64, u64)>,
}

pub struct ChannelManager {
    params: ChannelParams,
    metrics: Arc<BatcherMetrics>,
    /// Buffered blocks not yet known-safe. Front is the oldest.
    blocks: VecDeque<L2BlockData>,
    /// Index into `blocks` of the next block to add to a channel.
    block_cursor: usize,
    /// Hash of the most recently added block (reorg detection).
    tip_hash: Option<B256>,
    /// Highest block number added.
    tip_number: Option<u64>,
    /// The single open channel.
    current: Option<Channel>,
    /// Closed channels with frames left to submit / confirm. Front first.
    queue: VecDeque<Channel>,
}

impl ChannelManager {
    pub fn new(params: ChannelParams, metrics: Arc<BatcherMetrics>) -> Self {
        Self {
            params,
            metrics,
            blocks: VecDeque::new(),
            block_cursor: 0,
            tip_hash: None,
            tip_number: None,
            current: None,
            queue: VecDeque::new(),
        }
    }

    /// Number of buffered blocks.
    pub fn pending_block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Highest L2 block number currently held in state (added to the manager).
    pub fn newest_block(&self) -> Option<u64> {
        self.tip_number
    }

    /// Whether there is any block buffered that has not yet been added to a
    /// channel.
    fn has_unchanneled_blocks(&self) -> bool {
        self.block_cursor < self.blocks.len()
    }

    /// Clear all state. Mirrors `channelManager.Clear`.
    pub fn clear(&mut self) {
        self.blocks.clear();
        self.block_cursor = 0;
        self.tip_hash = None;
        self.tip_number = None;
        self.current = None;
        self.queue.clear();
    }

    /// Add an L2 block. Mirrors `channelManager.AddL2Block` (reorg check +
    /// enqueue).
    pub fn add_l2_block(&mut self, block: L2BlockData) -> Result<(), ChannelManagerError> {
        if let Some(tip) = self.tip_hash
            && block.parent_hash != tip
        {
            return Err(ChannelManagerError::Reorg {
                number: block.number,
                parent: block.parent_hash,
                tip,
            });
        }
        self.tip_hash = Some(block.hash);
        self.tip_number = Some(block.number);
        self.blocks.push_back(block);
        Ok(())
    }

    /// Ensure there is an open channel; create one if needed.
    fn ensure_current(&mut self, current_l1: u64) {
        if self.current.is_none() {
            self.current = Some(Channel::new(self.params.max_rlp_bytes, current_l1));
        }
    }

    /// Add buffered blocks to the current channel until it is full or the buffer
    /// is exhausted. Mirrors `channelManager.processBlocks`.
    fn process_blocks(&mut self) -> Result<(), ChannelManagerError> {
        while self.block_cursor < self.blocks.len() {
            let block = self.blocks[self.block_cursor].clone();
            let ch = self.current.as_mut().expect("ensure_current called");
            let added = ch.add_block(&block)?;
            if added {
                self.block_cursor += 1;
                // Close eagerly once the compressed target is reached.
                if ch.target_reached(self.params.target_output, self.params.approx_compr_ratio) {
                    ch.force_full(FullReason::TargetReached);
                    break;
                }
            } else {
                // Channel full; leave this block for the next channel.
                break;
            }
        }
        Ok(())
    }

    /// Drive channel construction and return the next frame to submit, if any.
    ///
    /// `force` requests closing the current channel even if it has not reached
    /// its target (used when the driver has caught up to the unsafe head or on
    /// an explicit flush). Mirrors `channelManager.TxData` + `getReadyChannel`.
    pub fn next_tx(
        &mut self,
        current_l1: u64,
        force: bool,
    ) -> Result<Option<PendingFrame>, ChannelManagerError> {
        // 1. If a queued channel already has a frame ready, send it first
        //    (preserve ordering: drain oldest channel fully before newer ones).
        if let Some(frame) = self.front_queue_frame() {
            return Ok(Some(frame));
        }

        // 2. Otherwise, build: fill the current channel and decide whether to
        //    close it.
        self.ensure_current(current_l1);
        self.process_blocks()?;

        let should_close = {
            let ch = self.current.as_ref().expect("ensure_current called");
            let caught_up = !self.has_unchanneled_blocks();
            ch.is_full()
                || ch.duration_elapsed(current_l1, self.params.max_channel_duration)
                || (force && caught_up && !ch.is_empty())
        };

        if should_close {
            let mut ch = self.current.take().expect("checked above");
            if ch.is_empty() {
                // Nothing to submit; drop the empty channel.
                return Ok(None);
            }
            ch.close_and_frame(self.params.max_frame_size)?;
            self.metrics.record_channel_closed();
            debug!(
                target: "exex::batcher::channel",
                channel_id = ?ch.id(),
                range = ?ch.block_range(),
                input_bytes = ch.input_bytes(),
                reason = ?ch.full_reason(),
                "closed channel",
            );
            self.queue.push_back(ch);
        }

        Ok(self.front_queue_frame())
    }

    /// Peek the next frame from the front (oldest) queued channel.
    fn front_queue_frame(&self) -> Option<PendingFrame> {
        let ch = self.queue.front()?;
        let frame = ch.peek_frame()?;
        Some(PendingFrame {
            channel_id: ch.id(),
            frame: frame.clone(),
            l2_range: ch.block_range(),
        })
    }

    /// Record that a frame was confirmed on L1. Mirrors `channelManager.TxConfirmed`.
    pub fn frame_confirmed(&mut self, channel_id: ChannelId, frame_number: u16) {
        if let Some(ch) = self.queue.front_mut()
            && ch.id() == channel_id
        {
            if ch.peek_frame().map(|f| f.frame_number) == Some(frame_number) {
                ch.confirm_frame();
            }
            if ch.is_fully_submitted() {
                self.queue.pop_front();
            }
        }
    }

    /// Record that a frame submission failed. The frame stays at the cursor and
    /// is retried on the next cycle. Mirrors `channelManager.TxFailed`.
    pub fn frame_failed(&mut self, channel_id: ChannelId, frame_number: u16) {
        warn!(
            target: "exex::batcher::channel",
            ?channel_id, frame_number, "frame submission failed; will retry",
        );
        // No cursor advance: peek_frame still returns the same frame.
    }

    /// Prune blocks at or below the safe head, and drop fully-submitted channels
    /// whose blocks are now safe. Mirrors `channelManager.PruneSafeBlocks` +
    /// `PruneChannels`.
    pub fn prune_safe(&mut self, safe_l2: u64) {
        while let Some(front) = self.blocks.front() {
            if front.number <= safe_l2 {
                self.blocks.pop_front();
                self.block_cursor = self.block_cursor.saturating_sub(1);
            } else {
                break;
            }
        }
        // Drop fully-submitted channels whose range is now safe.
        while let Some(ch) = self.queue.front() {
            match ch.block_range() {
                Some((_, end)) if end <= safe_l2 && ch.is_fully_submitted() => {
                    self.queue.pop_front();
                }
                _ => break,
            }
        }
    }
}
