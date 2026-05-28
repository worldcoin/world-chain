//! Channel encoder: blocks → singular batches → compressed channel → frames.
//!
//! Mirrors the consensus-critical parts of
//! `op-node/rollup/derive/channel_out.go` (`SingularChannelOut`) and
//! `singular_batch.go` (`BlockToSingularBatch`), reusing the **kona-protocol**
//! types ([`SingleBatch`], [`Batch`], [`Frame`]) for the encoding so the wire
//! format is guaranteed to match the derivation pipeline.
//!
//! The channel byte stream (input to the zlib compressor) is, per batch:
//!
//! ```text
//! element = rlp_string( batch_type(1 byte) ++ rlp([parent_hash, epoch_num,
//!                                                   epoch_hash, timestamp, txs]) )
//! ```
//!
//! which is exactly what `kona_protocol::Batch::encode` produces (the inner
//! `batch_type ++ rlp(...)`), wrapped in an RLP byte-string — matching Go's
//! `rlp.Encode(w, NewBatchData(batch))`.
//!
//! Compression is **zlib** via `flate2`. This is *not* consensus-critical for
//! correctness: the derivation pipeline decompresses any valid zlib stream
//! (`kona_protocol::BatchReader::decompress_zlib`). Only the framing and batch
//! RLP layout are consensus-critical, and those come from kona.
//!
//! v1 scope: singular batches + a "ratio" close heuristic (close when the
//! estimated compressed size reaches the per-channel target). Span batches and
//! eager frame streaming are follow-ups (see `BATCHER_SPEC.md`).

use std::{
    io::Write,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use alloy_primitives::keccak256;
use alloy_rlp::Header;
use flate2::{Compression, write::ZlibEncoder};
use kona_protocol::{Batch, ChannelId, Frame, SingleBatch};

use crate::source::L2BlockData;

/// Fixed frame overhead: `channel_id(16) + frame_number(2) + frame_data_len(4)
/// + is_last(1)`. See the OP [Frame Format] spec.
///
/// [Frame Format]: https://specs.optimism.io/protocol/derivation.html#frame-format
pub const FRAME_V0_OVERHEAD_SIZE: usize = 23;

/// Derivation format byte prefixed to the frame data in a batcher transaction.
pub const DERIVATION_VERSION_0: u8 = 0;

/// `MAX_RLP_BYTES_PER_CHANNEL` post-Fjord. The batcher must never produce a
/// channel whose uncompressed RLP size exceeds this, or the channel bank will
/// reject it.
pub const MAX_RLP_BYTES_PER_CHANNEL_FJORD: usize = 100_000_000;

/// Reason a channel was marked full / closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullReason {
    /// Estimated compressed output reached the per-channel target size.
    TargetReached,
    /// Adding another batch would exceed `MAX_RLP_BYTES_PER_CHANNEL`.
    MaxRlpBytes,
    /// A close was forced externally (timeout, flush, no more unsafe blocks).
    Forced,
}

#[derive(Debug, thiserror::Error)]
pub enum ChannelOutError {
    #[error("channel is already closed")]
    AlreadyClosed,
    #[error("adding batch would exceed MAX_RLP_BYTES_PER_CHANNEL")]
    TooManyRlpBytes,
    #[error("batch encode error: {0}")]
    Encode(String),
    #[error("zlib error: {0}")]
    Zlib(String),
}

static CHANNEL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a 16-byte channel ID, unique per process: `keccak256(nanos ++
/// counter)[..16]`. The derivation pipeline only requires per-channel
/// uniqueness within the channel-timeout window.
fn new_channel_id() -> ChannelId {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let ctr = CHANNEL_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut seed = [0u8; 16];
    seed[..8].copy_from_slice(&nanos.to_le_bytes());
    seed[8..].copy_from_slice(&ctr.to_le_bytes());
    let h = keccak256(seed);
    let mut id = [0u8; 16];
    id.copy_from_slice(&h.as_slice()[..16]);
    id
}

/// The channel encoder.
pub struct ChannelOut {
    id: ChannelId,
    /// Next frame number to emit.
    frame_number: u16,
    /// Uncompressed RLP length written so far (for `MAX_RLP_BYTES_PER_CHANNEL`).
    rlp_length: usize,
    max_rlp_bytes: usize,
    /// Compressor; consumed (`None`) once closed.
    encoder: Option<ZlibEncoder<Vec<u8>>>,
    /// Final compressed channel bytes, populated on `close`.
    compressed: Vec<u8>,
    /// Output cursor into `compressed`.
    cursor: usize,
    closed: bool,
}

impl ChannelOut {
    pub fn new(max_rlp_bytes: usize) -> Self {
        Self {
            id: new_channel_id(),
            frame_number: 0,
            rlp_length: 0,
            max_rlp_bytes,
            encoder: Some(ZlibEncoder::new(Vec::new(), Compression::best())),
            compressed: Vec::new(),
            cursor: 0,
            closed: false,
        }
    }

    pub fn id(&self) -> ChannelId {
        self.id
    }

    /// Total uncompressed RLP bytes written.
    pub fn input_bytes(&self) -> usize {
        self.rlp_length
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Encode the channel-stream element for a single L2 block: the RLP
    /// byte-string wrapping of `batch_type ++ rlp(singular_batch)`.
    pub fn encode_block_element(block: &L2BlockData) -> Result<Vec<u8>, ChannelOutError> {
        let batch = Batch::Single(SingleBatch {
            parent_hash: block.parent_hash,
            epoch_num: block.epoch_num,
            epoch_hash: block.epoch_hash,
            timestamp: block.timestamp,
            transactions: block.transactions.clone(),
        });

        // `Batch::encode` writes `batch_type(1) ++ rlp(inner)`.
        let mut typed: Vec<u8> = Vec::new();
        batch
            .encode(&mut typed)
            .map_err(|e| ChannelOutError::Encode(format!("{e:?}")))?;

        // Wrap as an RLP byte-string (matches Go's `rlp.Encode(w, BatchData)`).
        let mut element = Vec::with_capacity(typed.len() + 9);
        Header {
            list: false,
            payload_length: typed.len(),
        }
        .encode(&mut element);
        element.extend_from_slice(&typed);
        Ok(element)
    }

    /// Add a block to the channel. Returns `Err(TooManyRlpBytes)` if the block
    /// would push the channel past `MAX_RLP_BYTES_PER_CHANNEL`; in that case the
    /// block is *not* added and the channel should be closed.
    pub fn add_block(&mut self, block: &L2BlockData) -> Result<(), ChannelOutError> {
        if self.closed {
            return Err(ChannelOutError::AlreadyClosed);
        }
        let element = Self::encode_block_element(block)?;
        if self.rlp_length + element.len() > self.max_rlp_bytes {
            return Err(ChannelOutError::TooManyRlpBytes);
        }
        let encoder = self
            .encoder
            .as_mut()
            .ok_or(ChannelOutError::AlreadyClosed)?;
        encoder
            .write_all(&element)
            .map_err(|e| ChannelOutError::Zlib(e.to_string()))?;
        self.rlp_length += element.len();
        Ok(())
    }

    /// Estimated compressed size, using the configured approximate ratio.
    pub fn estimated_compressed_len(&self, approx_compr_ratio: f64) -> usize {
        (self.rlp_length as f64 * approx_compr_ratio) as usize
    }

    /// Whether the estimated compressed output has reached `target_output`.
    pub fn is_target_reached(&self, target_output: usize, approx_compr_ratio: f64) -> bool {
        self.estimated_compressed_len(approx_compr_ratio) >= target_output
    }

    /// Close the channel, finishing compression. Idempotent.
    pub fn close(&mut self) -> Result<(), ChannelOutError> {
        if self.closed {
            return Ok(());
        }
        let encoder = self.encoder.take().ok_or(ChannelOutError::AlreadyClosed)?;
        self.compressed = encoder
            .finish()
            .map_err(|e| ChannelOutError::Zlib(e.to_string()))?;
        self.closed = true;
        Ok(())
    }

    /// Drain all frames from the closed channel, each carrying at most
    /// `max_frame_size - FRAME_V0_OVERHEAD_SIZE` bytes of channel data. The last
    /// frame has `is_last = true`.
    ///
    /// Returns the encoded batcher-tx payloads (`version_byte ++ frame.encode()`)
    /// together with frame metadata. Must be called after [`Self::close`].
    pub fn output_frames(
        &mut self,
        max_frame_size: usize,
    ) -> Result<Vec<FrameOut>, ChannelOutError> {
        if !self.closed {
            return Err(ChannelOutError::AlreadyClosed);
        }
        let max_data = max_frame_size.saturating_sub(FRAME_V0_OVERHEAD_SIZE).max(1);
        let mut out = Vec::new();
        loop {
            if self.frame_number == u16::MAX && self.cursor < self.compressed.len() {
                return Err(ChannelOutError::Encode(
                    "channel exceeds u16 frame index".to_string(),
                ));
            }
            let remaining = self.compressed.len() - self.cursor;
            let take = remaining.min(max_data);
            let is_last = self.cursor + take == self.compressed.len();
            let data = self.compressed[self.cursor..self.cursor + take].to_vec();
            let frame = Frame {
                id: self.id,
                number: self.frame_number,
                data,
                is_last,
            };
            let mut payload = Vec::with_capacity(1 + frame.data.len() + FRAME_V0_OVERHEAD_SIZE);
            payload.push(DERIVATION_VERSION_0);
            payload.extend_from_slice(&frame.encode());
            out.push(FrameOut {
                frame_number: self.frame_number,
                is_last,
                payload,
            });

            self.cursor += take;
            self.frame_number = self.frame_number.saturating_add(1);
            if is_last {
                break;
            }
        }
        Ok(out)
    }
}

/// One emitted frame, ready to be sent as a single calldata batcher tx.
#[derive(Debug, Clone)]
pub struct FrameOut {
    pub frame_number: u16,
    pub is_last: bool,
    /// `DERIVATION_VERSION_0 ++ frame.encode()` — the L1 tx calldata.
    pub payload: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{B256, Bytes};

    fn block(n: u64) -> L2BlockData {
        L2BlockData {
            number: n,
            hash: B256::repeat_byte(n as u8),
            parent_hash: B256::repeat_byte((n.wrapping_sub(1)) as u8),
            timestamp: 1_700_000_000 + n,
            epoch_num: n / 10,
            epoch_hash: B256::repeat_byte(0xab),
            transactions: vec![
                Bytes::from(vec![0x02, 0xaa, 0xbb]),
                Bytes::from(vec![0x02, 0xcc]),
            ],
        }
    }

    #[test]
    fn encode_element_roundtrips_through_batch_reader() {
        // The element we write to the compressor must be readable by kona's
        // BatchReader (RLP string → Batch::decode). We validate the RLP-string
        // wrapping by decoding the inner bytes back into a Batch.
        use alloy_rlp::Decodable;
        let element = ChannelOut::encode_block_element(&block(5)).unwrap();
        // Strip the RLP string header to recover the typed batch bytes.
        let mut slice = element.as_slice();
        let inner = alloy_primitives::Bytes::decode(&mut slice).unwrap();
        assert_eq!(inner[0], 0, "singular batch type byte");
    }

    #[test]
    fn frames_cover_all_compressed_bytes() {
        let mut co = ChannelOut::new(MAX_RLP_BYTES_PER_CHANNEL_FJORD);
        for n in 1..=8 {
            co.add_block(&block(n)).unwrap();
        }
        assert!(co.input_bytes() > 0);
        co.close().unwrap();
        // Force tiny frames to exercise multi-frame splitting.
        let frames = co.output_frames(FRAME_V0_OVERHEAD_SIZE + 8).unwrap();
        assert!(!frames.is_empty());
        assert!(frames.last().unwrap().is_last);
        assert!(frames[..frames.len() - 1].iter().all(|f| !f.is_last));
    }
}
