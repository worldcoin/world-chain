use alloy_primitives::{Bytes, Signature, bytes::BufMut};
use alloy_rlp::{Decodable, Encodable};
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
    /// Length of the RLP encoding of the signature as a single byte-string item
    /// (byte-string head + body).
    pub(crate) fn payload_encoded_len(&self) -> usize {
        self.signature.length()
    }

    /// RLP-encodes the signature as a single byte-string item (no extra wrapping).
    pub(crate) fn encode_payload_raw(&self, out: &mut dyn BufMut) {
        self.signature.encode(out);
    }

    /// RLP-decodes the signature from a single byte-string item.
    pub(crate) fn decode_payload_raw(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let signature: Bytes = Decodable::decode(buf)?;
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
