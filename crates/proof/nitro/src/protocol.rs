//! Wire protocol shared by the Nitro host and the in-enclave guest binary.
//!
//! Framing is intentionally trivial: each message is `u32` big-endian length followed by a
//! CBOR-encoded [`EnclaveRequest`] or [`EnclaveResponse`]. We deliberately avoid using rkyv
//! for the outer envelope so that the protocol stays decoupled from the inner witness format
//! and can be evolved (versioned) without re-generating archived layouts. The inner witness
//! payload is rkyv-encoded [`WorldRangeWitnessData`] because that is what the SP1 range
//! program already consumes.

use alloy_primitives::B256;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use world_chain_proof_succinct_client_utils::{
    WorldRangeProofPublicValues, boot::BootInfoStruct, types::AggregationInputs,
};

/// Current protocol version. Bumped whenever the wire format changes incompatibly.
pub const PROTOCOL_VERSION: u32 = 1;

/// Default vsock port the enclave binary listens on.
pub const DEFAULT_VSOCK_PORT: u32 = 5005;

/// Maximum frame size accepted on the wire (64 MiB).
pub const MAX_FRAME_BYTES: u32 = 64 * 1024 * 1024;

/// Requests the host can send to the enclave.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnclaveRequest {
    /// Prove a single range transition.
    Range {
        /// Protocol version the host expects.
        version: u32,
        /// rkyv-serialized `WorldRangeWitnessData`.
        witness_rkyv: Vec<u8>,
        /// Optional host-computed public values the enclave must match before signing.
        expected_public_values: Option<WorldRangeProofPublicValues>,
    },
    /// Prove an aggregation over previously-attested range boot infos.
    Aggregation {
        /// Protocol version the host expects.
        version: u32,
        /// Same aggregation inputs as the Succinct backend.
        inputs: AggregationInputs,
        /// CBOR-encoded L1 headers, ordered from oldest to newest.
        l1_headers_cbor: Vec<u8>,
    },
}

/// Responses the enclave can return to the host.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnclaveResponse {
    /// Successful range proof.
    Range {
        /// Boot info committed by the enclave.
        boot_info: BootInfoStruct,
        /// `COSE_Sign1` attestation document bytes from the NSM device.
        attestation_doc: Vec<u8>,
    },
    /// Successful aggregation proof.
    Aggregation {
        /// Boot info derived from the aggregated range boot infos.
        boot_info: BootInfoStruct,
        /// Attestation document from the NSM device.
        attestation_doc: Vec<u8>,
    },
    /// Enclave-side error. The host treats this as a permanent failure for the request.
    Error {
        /// Human-readable error message produced inside the enclave.
        message: String,
    },
}

/// Computes the 32-byte `user_data` that the enclave embeds in the attestation request when
/// signing a range proof. Binding this commits the attestation to the exact transition.
///
/// `SHA256( l2_pre_root || l2_post_root || l2_block_number_be || rollup_config_hash )`
#[must_use]
pub fn range_user_data(boot_info: &BootInfoStruct) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(boot_info.l2PreRoot.as_slice());
    hasher.update(boot_info.l2PostRoot.as_slice());
    hasher.update(boot_info.l2BlockNumber.to_be_bytes());
    hasher.update(boot_info.rollupConfigHash.as_slice());
    let out = hasher.finalize();
    let mut user_data = [0u8; 32];
    user_data.copy_from_slice(out.as_slice());
    user_data
}

/// Computes the `user_data` for aggregation proofs.
///
/// `SHA256( l2_pre_root || l2_post_root || l2_block_number_be || rollup_config_hash ||
///          multi_block_vkey_be || prover_address )`
#[must_use]
pub fn aggregation_user_data(boot_info: &BootInfoStruct, inputs: &AggregationInputs) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(boot_info.l2PreRoot.as_slice());
    hasher.update(boot_info.l2PostRoot.as_slice());
    hasher.update(boot_info.l2BlockNumber.to_be_bytes());
    hasher.update(boot_info.rollupConfigHash.as_slice());
    for word in inputs.multi_block_vkey {
        hasher.update(word.to_be_bytes());
    }
    hasher.update(inputs.prover_address.as_slice());
    let out = hasher.finalize();
    let mut user_data = [0u8; 32];
    user_data.copy_from_slice(out.as_slice());
    user_data
}

/// Writes a length-prefixed CBOR frame.
pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), FrameError>
where
    W: AsyncWriteExt + Unpin,
    T: Serialize,
{
    let mut buf = Vec::with_capacity(1024);
    ciborium::into_writer(value, &mut buf).map_err(|err| FrameError::Encode(err.to_string()))?;
    let len: u32 = buf
        .len()
        .try_into()
        .map_err(|_| FrameError::FrameTooLarge(buf.len()))?;
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::FrameTooLarge(buf.len()));
    }
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&buf).await?;
    writer.flush().await?;
    Ok(())
}

/// Reads a length-prefixed CBOR frame.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<T, FrameError>
where
    R: AsyncReadExt + Unpin,
    T: for<'de> Deserialize<'de>,
{
    let mut len_bytes = [0u8; 4];
    reader.read_exact(&mut len_bytes).await?;
    let len = u32::from_be_bytes(len_bytes);
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::FrameTooLarge(len as usize));
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    let value =
        ciborium::from_reader(buf.as_slice()).map_err(|err| FrameError::Decode(err.to_string()))?;
    Ok(value)
}

/// Errors raised by the wire-framing helpers.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// IO error reading or writing the underlying transport.
    #[error("vsock io error: {0}")]
    Io(#[from] std::io::Error),
    /// CBOR encoding failed.
    #[error("frame encode failed: {0}")]
    Encode(String),
    /// CBOR decoding failed.
    #[error("frame decode failed: {0}")]
    Decode(String),
    /// Frame exceeds the maximum allowed size.
    #[error("frame too large: {0} bytes")]
    FrameTooLarge(usize),
}

/// Hashes an arbitrary byte slice and returns it as a 32-byte `B256`.
#[must_use]
pub fn sha256_b256(bytes: &[u8]) -> B256 {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    B256::from_slice(hasher.finalize().as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256};

    fn boot_info() -> BootInfoStruct {
        BootInfoStruct {
            l1Head: B256::from([1; 32]),
            l2PreRoot: B256::from([2; 32]),
            l2PostRoot: B256::from([3; 32]),
            l2BlockNumber: 42,
            rollupConfigHash: B256::from([4; 32]),
        }
    }

    #[test]
    fn range_user_data_is_deterministic() {
        let a = range_user_data(&boot_info());
        let b = range_user_data(&boot_info());
        assert_eq!(a, b);
    }

    #[test]
    fn range_user_data_depends_on_post_root() {
        let mut bi = boot_info();
        let original = range_user_data(&bi);
        bi.l2PostRoot = B256::from([9; 32]);
        let mutated = range_user_data(&bi);
        assert_ne!(original, mutated);
    }

    #[test]
    fn aggregation_user_data_depends_on_prover_address() {
        let inputs_a = AggregationInputs {
            boot_infos: vec![boot_info()],
            latest_l1_checkpoint_head: B256::from([5; 32]),
            multi_block_vkey: [1; 8],
            prover_address: Address::from([6; 20]),
        };
        let mut inputs_b = inputs_a.clone();
        inputs_b.prover_address = Address::from([7; 20]);

        assert_ne!(
            aggregation_user_data(&boot_info(), &inputs_a),
            aggregation_user_data(&boot_info(), &inputs_b),
        );
    }
}
