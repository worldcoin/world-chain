//! A single channel: an open [`ChannelOut`] plus block-range and frame-output
//! tracking.
//!
//! Mirrors the union of `op-batcher/batcher/channel_builder.go` (`ChannelBuilder`
//! — block accumulation + close timeouts) and `channel.go` (`channel` — frame
//! output cursor + tx tracking). The v1 model closes the channel then emits all
//! frames at once (`closeAndOutputAllFrames`); eager frame streaming is a
//! follow-up.

use kona_protocol::ChannelId;

use crate::{
    channel_out::{ChannelOut, ChannelOutError, FrameOut, FullReason},
    source::L2BlockData,
};

/// Per-channel state.
pub struct Channel {
    out: ChannelOut,
    /// First / last L2 block number added to this channel.
    l2_start: Option<u64>,
    l2_end: Option<u64>,
    /// L1 block number when the channel was opened (for the duration timeout).
    open_l1_block: u64,
    /// Closed-channel frames, in order. Empty until [`Self::close_and_frame`].
    frames: Vec<FrameOut>,
    /// Index of the next frame to hand to the driver. Advances only on confirm,
    /// so a failed frame is retried (preserving contiguous ordering — Holocene).
    frame_cursor: usize,
    full: bool,
    full_reason: Option<FullReason>,
}

impl Channel {
    pub fn new(max_rlp_bytes: usize, open_l1_block: u64) -> Self {
        Self {
            out: ChannelOut::new(max_rlp_bytes),
            l2_start: None,
            l2_end: None,
            open_l1_block,
            frames: Vec::new(),
            frame_cursor: 0,
            full: false,
            full_reason: None,
        }
    }

    pub fn id(&self) -> ChannelId {
        self.out.id()
    }

    pub fn is_full(&self) -> bool {
        self.full
    }

    pub fn full_reason(&self) -> Option<FullReason> {
        self.full_reason
    }

    pub fn is_empty(&self) -> bool {
        self.l2_start.is_none()
    }

    pub fn block_range(&self) -> Option<(u64, u64)> {
        Some((self.l2_start?, self.l2_end?))
    }

    fn mark_full(&mut self, reason: FullReason) {
        if !self.full {
            self.full = true;
            self.full_reason = Some(reason);
        }
    }

    /// Try to add a block. Returns `Ok(true)` if added, `Ok(false)` if the
    /// channel is full and the block was *not* added (caller should close this
    /// channel and start a new one for the block).
    pub fn add_block(&mut self, block: &L2BlockData) -> Result<bool, ChannelOutError> {
        if self.full || self.out.is_closed() {
            return Ok(false);
        }
        match self.out.add_block(block) {
            Ok(()) => {
                self.l2_start.get_or_insert(block.number);
                self.l2_end = Some(block.number);
                Ok(true)
            }
            Err(ChannelOutError::TooManyRlpBytes) => {
                self.mark_full(FullReason::MaxRlpBytes);
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    /// Whether the channel's estimated compressed size reached the target.
    pub fn target_reached(&self, target_output: usize, approx_compr_ratio: f64) -> bool {
        self.out
            .is_target_reached(target_output, approx_compr_ratio)
    }

    /// Whether the channel should be closed at `current_l1` given the max
    /// channel duration (0 = disabled).
    ///
    /// Mirrors `ChannelBuilder.updateDurationTimeout` /
    /// `(*ChannelBuilder).TimedOut`.
    pub fn duration_elapsed(&self, current_l1: u64, max_channel_duration: u64) -> bool {
        max_channel_duration != 0 && current_l1 >= self.open_l1_block + max_channel_duration
    }

    /// Mark this channel full for the given reason if it is not already.
    pub fn force_full(&mut self, reason: FullReason) {
        self.mark_full(reason);
    }

    /// Close the channel and produce all frames.
    ///
    /// Mirrors `ChannelBuilder.OutputFrames` → `closeAndOutputAllFrames`.
    pub fn close_and_frame(&mut self, max_frame_size: usize) -> Result<(), ChannelOutError> {
        if !self.full {
            self.mark_full(FullReason::Forced);
        }
        self.out.close()?;
        self.frames = self.out.output_frames(max_frame_size)?;
        Ok(())
    }

    /// Peek the next frame to submit, without advancing.
    pub fn peek_frame(&self) -> Option<&FrameOut> {
        self.frames.get(self.frame_cursor)
    }

    /// Advance past the current frame after it has been confirmed on L1.
    ///
    /// Mirrors `channel.TxConfirmed` advancing past the confirmed frame.
    pub fn confirm_frame(&mut self) {
        if self.frame_cursor < self.frames.len() {
            self.frame_cursor += 1;
        }
    }

    /// Whether all frames have been submitted and confirmed.
    pub fn is_fully_submitted(&self) -> bool {
        self.out.is_closed() && self.frame_cursor >= self.frames.len() && !self.frames.is_empty()
    }

    pub fn input_bytes(&self) -> usize {
        self.out.input_bytes()
    }
}
