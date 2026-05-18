use alloy_primitives::{Bytes, Signature, bytes::BufMut};
use alloy_rlp::{Decodable, Encodable, Header};
use core::hash::Hash;

/// The EIP-2718 transaction type byte for WIP-1001 transactions.
pub const WIP_1001_TX_TYPE: u8 = 0x1D;

/// Signature scheme for a [`TxWip1001`](crate::transaction::TxWip1001).
///
/// The WIP-1001 envelope doesn't put any restriction on the signature, that is opaque
/// to the protocol and it's interpreted and validated directly by the session verifier
/// contract through the `sessionVerifier.isValidSignature(signingHash, signature)` fn.
#[derive(Debug, Clone, Hash, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Wip1001Signature {
    /// The opaque signature bytes.
    pub signature: Bytes,
}

impl Wip1001Signature {
    /// Length of the RLP-encoded `signature_payload` bytes (no outer string header).
    pub(crate) fn payload_encoded_len(&self) -> usize {
        Header {
            list: true,
            payload_length: self.signature.length(),
        }
        .length_with_payload()
    }

    /// Encodes the `signature_payload` into `out` without an outer RLP string header.
    pub(crate) fn encode_payload_raw(&self, out: &mut dyn BufMut) {
        Header {
            list: true,
            payload_length: self.signature.length(),
        }
        .encode(out);
        self.signature.encode(out);
    }

    /// Decodes a `signature_payload` byte string.
    ///
    /// The caller has already consumed the outer RLP string header around
    /// `signature_payload`; this reads the raw payload bytes.
    pub(crate) fn decode_payload_raw(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let header = Header::decode(buf)?;
        if !header.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let start = buf.len();
        let signature: Bytes = Decodable::decode(buf)?;
        let consumed = start - buf.len();
        if consumed != header.payload_length {
            return Err(alloy_rlp::Error::ListLengthMismatch {
                expected: header.payload_length,
                got: consumed,
            });
        }
        Ok(Self { signature })
    }
}

impl From<Signature> for Wip1001Signature {
    fn from(value: Signature) -> Self {
        let signature = value.as_bytes().into();
        Self { signature }
    }
}

impl From<&Signature> for Wip1001Signature {
    fn from(value: &Signature) -> Self {
        let signature = value.as_bytes().into();
        Self { signature }
    }
}
