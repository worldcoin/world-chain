//! COSE_Sign1 attestation decoding helpers.
//!
//! These are pure, platform-independent functions used off-chain to reconstruct the exact
//! bytes an AWS Nitro attestation document is signed over (the "TBS" / `Sig_Structure`) and
//! to extract the raw P-384 signature. The reconstructed TBS matches the bytes produced by
//! the on-chain `NitroValidator.decodeAttestationTbs` (base/nitro-validator), so it can be
//! fed directly to `NitroEnclaveKeyRegistry.registerKey` /
//! `NitroAttestationVerifier.verifyAttestation`.

use anyhow::{Result, bail};
use coset::{CborSerializable as _, CoseSign1, TaggedCborSerializable as _};

/// Length in bytes of a P-384 COSE ES384 signature (`r || s`, 48 bytes each).
pub const P384_SIGNATURE_LEN: usize = 96;

/// Errors raised while decoding the outer `COSE_Sign1` envelope.
#[derive(Debug, thiserror::Error)]
pub enum CoseDecodeError {
    /// The bytes are not a well-formed `COSE_Sign1` structure.
    #[error("COSE_Sign1 decode failed: {0:?}")]
    Malformed(coset::CoseError),
    /// The `payload` element is CBOR `nil`. Every NSM attestation document carries one.
    #[error("COSE_Sign1 payload is absent")]
    MissingPayload,
}

/// Decodes the `COSE_Sign1` envelope, accepting both the bare array the NSM emits and the
/// RFC 8152 tag-18 wrapping, and rejecting an absent payload.
///
/// `coset` retains the original `protected` bstr, so [`CoseSign1::tbs_data`] reproduces the
/// bytes the NSM signed rather than a re-encoding — which is what keeps the TBS byte-identical
/// to the on-chain `NitroValidator.decodeAttestationTbs`.
pub fn parse_cose_sign1(raw: &[u8]) -> Result<CoseSign1, CoseDecodeError> {
    let sign1 = CoseSign1::from_tagged_slice(raw)
        .or_else(|_| CoseSign1::from_slice(raw))
        .map_err(CoseDecodeError::Malformed)?;
    if sign1.payload.is_none() {
        return Err(CoseDecodeError::MissingPayload);
    }
    Ok(sign1)
}

/// Decodes a raw `COSE_Sign1` Nitro attestation document into `(attestation_tbs, signature)`.
///
/// - `attestation_tbs` is the CBOR encoding of the RFC 8152 `Sig_Structure`
///   `["Signature1", protected, h'', payload]`. This is exactly the byte string that the
///   NSM signed and that the on-chain `NitroValidator.decodeAttestationTbs` reconstructs, so
///   `keccak256`/`sha384` over it matches the on-chain view.
/// - `signature` is the 96-byte `r || s` P-384 signature from the fourth COSE_Sign1 element.
///
/// # Errors
///
/// Returns an error if the input is not a well-formed `COSE_Sign1` structure or if the
/// signature is not exactly [`P384_SIGNATURE_LEN`] bytes.
pub fn decode_attestation_tbs(raw: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let sign1 = parse_cose_sign1(raw)?;

    if sign1.signature.len() != P384_SIGNATURE_LEN {
        bail!(
            "attestation signature must be {P384_SIGNATURE_LEN} bytes, got {}",
            sign1.signature.len()
        );
    }

    Ok((sign1.tbs_data(&[]), sign1.signature))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ciborium::value::Value;

    /// Builds a minimal COSE_Sign1 array `[protected, unprotected, payload, signature]`.
    fn make_cose(protected: &[u8], payload: &[u8], signature: &[u8]) -> Vec<u8> {
        let cose = Value::Array(vec![
            Value::Bytes(protected.to_vec()),
            Value::Map(Vec::new()),
            Value::Bytes(payload.to_vec()),
            Value::Bytes(signature.to_vec()),
        ]);
        let mut out = Vec::new();
        ciborium::ser::into_writer(&cose, &mut out).unwrap();
        out
    }

    /// The construction this module used before `coset`, and the one the on-chain
    /// `NitroValidator.decodeAttestationTbs` agrees with.
    fn reference_tbs(protected: &[u8], payload: &[u8]) -> Vec<u8> {
        let tbs = Value::Array(vec![
            Value::Text("Signature1".to_string()),
            Value::Bytes(protected.to_vec()),
            Value::Bytes(Vec::new()),
            Value::Bytes(payload.to_vec()),
        ]);
        let mut out = Vec::new();
        ciborium::ser::into_writer(&tbs, &mut out).unwrap();
        out
    }

    /// The property `registerKey` depends on: byte-for-byte equality with the old construction.
    #[test]
    fn tbs_matches_the_hand_rolled_construction() {
        let protected = [0xa1, 0x01, 0x38, 0x22]; // {1: -35} (ES384)
        let payload = b"hello-attestation-payload";

        let doc = make_cose(&protected, payload, &[7u8; P384_SIGNATURE_LEN]);
        let (tbs, _) = decode_attestation_tbs(&doc).unwrap();

        assert_eq!(tbs, reference_tbs(&protected, payload));
    }

    /// A signature made over the reference `Sig_Structure` must verify through `coset`'s path,
    /// or [`crate::attestation::verify_cose_sign1_signature`] cannot work at all.
    #[test]
    fn coset_tbs_verifies_a_real_p384_signature() {
        use p384::ecdsa::{
            Signature, SigningKey,
            signature::{Signer as _, Verifier as _},
        };

        let protected = [0xa1, 0x01, 0x38, 0x22];
        let payload = b"hello-attestation-payload";

        let signing_key = SigningKey::random(&mut rand::thread_rng());
        let signature: Signature = signing_key.sign(&reference_tbs(&protected, payload));

        let doc = make_cose(&protected, payload, &signature.to_bytes());
        let sign1 = parse_cose_sign1(&doc).unwrap();

        let verifying_key = signing_key.verifying_key();
        sign1
            .verify_signature(&[], |sig, tbs| {
                verifying_key.verify(tbs, &Signature::from_slice(sig)?)
            })
            .unwrap();
    }

    /// RFC 8152 permits the tag-18 wrapping; the NSM omits it. Both must decode to the same TBS.
    #[test]
    fn accepts_tag_18_wrapping() {
        let protected = [0xa1, 0x01, 0x38, 0x22];
        let payload = b"hello-attestation-payload";
        let signature = vec![7u8; P384_SIGNATURE_LEN];

        let bare = make_cose(&protected, payload, &signature);
        let untagged: Value = ciborium::de::from_reader(bare.as_slice()).unwrap();
        let mut tagged = Vec::new();
        ciborium::ser::into_writer(&Value::Tag(18, Box::new(untagged)), &mut tagged).unwrap();

        assert_eq!(
            decode_attestation_tbs(&tagged).unwrap(),
            decode_attestation_tbs(&bare).unwrap()
        );
    }

    /// `tbs_data` treats an absent payload as empty, silently signing over nothing.
    #[test]
    fn rejects_absent_payload() {
        let cose = Value::Array(vec![
            Value::Bytes(vec![0xa0]),
            Value::Map(Vec::new()),
            Value::Null,
            Value::Bytes(vec![7u8; P384_SIGNATURE_LEN]),
        ]);
        let mut doc = Vec::new();
        ciborium::ser::into_writer(&cose, &mut doc).unwrap();

        let err = decode_attestation_tbs(&doc).unwrap_err();
        assert!(err.to_string().contains("payload is absent"), "got: {err}");
    }

    #[test]
    fn decodes_tbs_and_signature() {
        let protected = [0xa1, 0x01, 0x38, 0x22]; // {1: -35} (ES384) style header bytes
        let payload = b"hello-attestation-payload";
        let signature = vec![7u8; P384_SIGNATURE_LEN];

        let doc = make_cose(&protected, payload, &signature);
        let (tbs, sig) = decode_attestation_tbs(&doc).unwrap();

        assert_eq!(sig, signature);

        // The reconstructed TBS must itself be a valid CBOR array of the expected shape.
        let decoded: Value = ciborium::de::from_reader(tbs.as_slice()).unwrap();
        match decoded {
            Value::Array(items) => {
                assert_eq!(items.len(), 4);
                assert_eq!(items[0], Value::Text("Signature1".to_string()));
                assert_eq!(items[1], Value::Bytes(protected.to_vec()));
                assert_eq!(items[2], Value::Bytes(Vec::new()));
                assert_eq!(items[3], Value::Bytes(payload.to_vec()));
            }
            other => panic!("expected array, got {other:?}"),
        }
    }

    #[test]
    fn rejects_wrong_signature_length() {
        let doc = make_cose(&[0xa0], b"payload", &[0u8; 64]);
        let err = decode_attestation_tbs(&doc).unwrap_err();
        assert!(err.to_string().contains("signature must be"));
    }

    #[test]
    fn rejects_non_cose() {
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&Value::Text("not-a-cose".into()), &mut buf).unwrap();
        assert!(decode_attestation_tbs(&buf).is_err());
    }
}
