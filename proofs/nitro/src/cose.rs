//! COSE_Sign1 attestation decoding helpers.
//!
//! These are pure, platform-independent functions used off-chain to reconstruct the exact
//! bytes an AWS Nitro attestation document is signed over (the "TBS" / `Sig_Structure`) and
//! to extract the raw P-384 signature. The reconstructed TBS matches the bytes produced by
//! the on-chain `NitroValidator.decodeAttestationTbs` (base/nitro-validator), so it can be
//! fed directly to `NitroEnclaveKeyRegistry.registerKey` /
//! `NitroAttestationVerifier.verifyAttestation`.

use anyhow::{Result, bail};
use ciborium::value::Value;

/// Length in bytes of a P-384 COSE ES384 signature (`r || s`, 48 bytes each).
pub const P384_SIGNATURE_LEN: usize = 96;

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
/// Returns an error if the input is not a well-formed 4-element `COSE_Sign1` structure or if
/// the signature is not exactly [`P384_SIGNATURE_LEN`] bytes.
pub fn decode_attestation_tbs(raw: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let value: Value = ciborium::de::from_reader(raw)
        .map_err(|e| anyhow::anyhow!("COSE_Sign1 CBOR decode error: {e:?}"))?;

    // A COSE_Sign1 document is a 4-element array, optionally wrapped in CBOR tag 18.
    let array = match value {
        Value::Tag(_, inner) => match *inner {
            Value::Array(a) => a,
            _ => bail!("expected CBOR array inside COSE_Sign1 tag"),
        },
        Value::Array(a) => a,
        _ => bail!("expected CBOR array or tag at COSE_Sign1 root"),
    };

    if array.len() != 4 {
        bail!("COSE_Sign1 must have 4 elements, got {}", array.len());
    }

    let protected = match &array[0] {
        Value::Bytes(b) => b.clone(),
        _ => bail!("COSE_Sign1 protected header must be a byte string"),
    };
    let payload = match &array[2] {
        Value::Bytes(b) => b.clone(),
        _ => bail!("COSE_Sign1 payload must be a byte string"),
    };
    let signature = match &array[3] {
        Value::Bytes(b) => b.clone(),
        _ => bail!("COSE_Sign1 signature must be a byte string"),
    };

    if signature.len() != P384_SIGNATURE_LEN {
        bail!(
            "attestation signature must be {P384_SIGNATURE_LEN} bytes, got {}",
            signature.len()
        );
    }

    // Reconstruct the Sig_Structure: ["Signature1", protected, h'', payload].
    // `Value::Bytes` always serialises to a definite-length, minimally-encoded byte string,
    // which matches how the NSM originally encoded the protected/payload elements and how the
    // on-chain `_constructAttestationTbs` slices them.
    let tbs = Value::Array(vec![
        Value::Text("Signature1".to_string()),
        Value::Bytes(protected),
        Value::Bytes(Vec::new()),
        Value::Bytes(payload),
    ]);
    let mut tbs_bytes = Vec::new();
    ciborium::ser::into_writer(&tbs, &mut tbs_bytes)
        .map_err(|e| anyhow::anyhow!("COSE_Sign1 TBS CBOR encode error: {e:?}"))?;

    Ok((tbs_bytes, signature))
}

#[cfg(test)]
mod tests {
    use super::*;

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
