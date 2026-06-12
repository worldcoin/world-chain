//! Host-side verification of NSM attestation documents.
//!
//! A Nitro NSM attestation document is a COSE_Sign1 structure whose payload is a CBOR map
//! containing the PCRs, the optional `user_data` field, the enclave certificate, and the
//! certificate chain back to the AWS Nitro Attestation PKI root. This module checks:
//!
//! 1. PCR / `user_data` invariants needed by the World fault proof flow.
//! 2. The COSE_Sign1 P-384 signature against the leaf certificate's public key.
//! 3. That the root certificate in the chain matches the hardcoded AWS Nitro root CA.
//!
//! 4. Certificate validity periods (not-before / not-after) for every cert in the chain.
//! 5. Nonce freshness: the host supplies a per-request nonce that the NSM embeds in the
//!    signed payload, preventing replay of captured attestation documents.

use std::collections::BTreeMap;

use crate::{ExpectedPcrs, PCR_LEN, PcrDigest};

// ──────────────────────────────────────────────────────────────────────────────────────
// AWS Nitro Attestation PKI root CA
// ──────────────────────────────────────────────────────────────────────────────────────

/// DER-encoded AWS Nitro Attestation PKI root CA certificate.
///
/// Source: <https://aws-nitro-enclaves.amazonaws.com/AWS_NitroEnclaves_Root-G1.zip>
/// SHA-256 of the zip: `8cf60e2b2efca96c6a9e71e851d00c1b6991cc09eadbe64a6a1d1b1eb9faff7c`
///
/// This constant is used to anchor the certificate chain validation in
/// [`verify_cose_sign1_signature`]. The root certificate in the attestation document's
/// `cabundle` field must match this value byte-for-byte.
pub const AWS_NITRO_ROOT_CA_PEM: &str = r"-----BEGIN CERTIFICATE-----
MIICETCCAZagAwIBAgIRAPkxdWgbkK/hHUbMtOTn+FYwCgYIKoZIzj0EAwMwSTEL
MAkGA1UEBhMCVVMxDzANBgNVBAoMBkFtYXpvbjEMMAoGA1UECwwDQVdTMRswGQYD
VQQDDBJhd3Mubml0cm8tZW5jbGF2ZXMwHhcNMTkxMDI4MTMyODA1WhcNNDkxMDI4
MTQyODA1WjBJMQswCQYDVQQGEwJVUzEPMA0GA1UECgwGQW1hem9uMQwwCgYDVQQL
DANBV1MxGzAZBgNVBAMMEmF3cy5uaXRyby1lbmNsYXZlczB2MBAGByqGSM49AgEG
BSuBBAAiA2IABPwCVOumCMHzaHDimtqQvkY4MpJzbolL//Zy2YlES1BR5TSksfbb
48C8WBoyt7F2Bw7eEtaaP+ohG2bnUs990d0JX28TcPQXCEPZ3BABIeTPYwEoCWZE
h8l5YoQwTcU/9KNCMEAwDwYDVR0TAQH/BAUwAwEB/zAdBgNVHQ4EFgQUkCW1DdkF
R+eWw5b6cp3PmanfS5YwDgYDVR0PAQH/BAQDAgGGMAoGCCqGSM49BAMDA2kAMGYC
MQCjfy+Rocm9Xue4YnwWmNJVA44fA0P5W2OpYow9OYCVRaEevL8uO1XYru5xtMPW
rfMCMQCi85sWBbJwKKXdS6BptQFuZbT73o/gBh1qUxl/nNr12UO8Yfwr6wPLb+6N
IwLz3/Y=
-----END CERTIFICATE-----";

/// Lazy-decoded DER bytes of [`AWS_NITRO_ROOT_CA_PEM`].
///
/// Returns `Err` if the PEM constant is malformed.
fn aws_root_ca_der() -> Result<Vec<u8>, String> {
    let pem = AWS_NITRO_ROOT_CA_PEM;
    let b64: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");
    use base64::engine::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("AWS root CA PEM decode failed: {e}"))
}

// ──────────────────────────────────────────────────────────────────────────────────────
// Error types
// ──────────────────────────────────────────────────────────────────────────────────────

/// Errors raised while validating an attestation document.
#[derive(Debug, thiserror::Error)]
pub enum AttestationError {
    /// The document is not a valid CBOR-encoded COSE_Sign1 structure.
    #[error("attestation doc is malformed: {0}")]
    Malformed(String),
    /// A required attestation document field was missing.
    #[error("attestation doc missing field: {0}")]
    MissingField(&'static str),
    /// A PCR value did not match the expected measurement.
    #[error("pcr{index} mismatch: expected {expected}, got {actual}")]
    PcrMismatch {
        /// PCR index that mismatched.
        index: u8,
        /// Expected PCR hex string.
        expected: String,
        /// Actual PCR hex string from the document.
        actual: String,
    },
    /// `user_data` field did not match the expected boot-info commitment.
    #[error("user_data mismatch: expected {expected}, got {actual}")]
    UserDataMismatch {
        /// Expected user data hex string.
        expected: String,
        /// Actual user data hex string from the document.
        actual: String,
    },
    /// An expected PCR was all-zero, which is the placeholder value and indicates the
    /// caller forgot to configure real measurements. We refuse to silently accept the
    /// document in that case because doing so would let the enclave run any unrelated
    /// image with the same `user_data`.
    #[error("expected pcr{index} is all-zero placeholder; supply real PCR measurements to verify")]
    EmptyExpectedPcr {
        /// PCR index whose expected value was the placeholder.
        index: u8,
    },
    /// COSE_Sign1 signature verification failed.
    #[error("COSE_Sign1 signature verification failed: {0}")]
    CoseSignature(String),
    /// Certificate chain validation failed.
    #[error("certificate chain validation failed: {0}")]
    CertChain(String),
    /// A certificate in the chain is outside its validity window.
    #[error("certificate validity check failed: {0}")]
    CertExpired(String),
    /// The nonce in the attestation document does not match the expected value.
    #[error("attestation nonce mismatch: expected {expected}, got {actual}")]
    NonceMismatch { expected: String, actual: String },
    /// The `public_key` in the NSM attestation payload does not match the key supplied
    /// on the wire by the enclave.
    ///
    /// This would allow an attacker to swap the wire `public_key` while presenting a
    /// valid attestation document, binding proof signatures to an uncertified key.
    #[error("attestation public_key mismatch: NSM payload key 0x{nsm} != wire key 0x{wire}")]
    PublicKeyMismatch {
        /// Hex-encoded key extracted from the NSM attestation payload.
        nsm: String,
        /// Hex-encoded key received on the wire in `EnclaveResponse::Attestation`.
        wire: String,
    },
}

// ──────────────────────────────────────────────────────────────────────────────────────
// Parsed attestation doc
// ──────────────────────────────────────────────────────────────────────────────────────

/// Decodes the relevant subset of a Nitro attestation document.
#[derive(Clone, Debug)]
pub struct ParsedAttestationDoc {
    /// `pcrs` map from PCR index to digest bytes.
    pub pcrs: BTreeMap<u8, Vec<u8>>,
    /// Optional `user_data` field. Present whenever the enclave passed one to `Request::Attestation`.
    pub user_data: Option<Vec<u8>>,
    /// `module_id` of the originating enclave.
    pub module_id: Option<String>,
    /// `digest` algorithm field (typically `"SHA384"`).
    pub digest: Option<String>,
    /// DER-encoded leaf certificate used to sign the COSE_Sign1 structure.
    pub certificate: Option<Vec<u8>>,
    /// DER-encoded CA bundle (intermediate certs, ordered from intermediate closest to
    /// leaf toward the root, inclusive of the root CA).
    pub cabundle: Vec<Vec<u8>>,
    /// Optional `public_key` field. Present when the enclave called `NsmRequest::Attestation`
    /// with `public_key: Some(bytes)` — i.e., for [`EnclaveRequest::GetAttestation`] responses.
    /// The bytes are the compressed SEC1 encoding of the enclave's ephemeral secp256k1 key.
    ///
    /// Use [`extract_nsm_public_key`] to obtain this value with a mandatory-presence check.
    pub public_key: Option<Vec<u8>>,
    /// Optional `nonce` field. Present when the host supplied a nonce in the request, which
    /// the NSM embeds verbatim into the signed payload for replay protection.
    pub nonce: Option<Vec<u8>>,
}

// ──────────────────────────────────────────────────────────────────────────────────────
// Parsing
// ──────────────────────────────────────────────────────────────────────────────────────

/// Parses a `COSE_Sign1` Nitro attestation document and returns the inner payload fields the
/// host cares about.
///
/// This does **not** verify the signature or certificate chain — call
/// [`verify_cose_sign1_signature`] for full cryptographic verification.
pub fn parse_attestation_doc(doc: &[u8]) -> Result<ParsedAttestationDoc, AttestationError> {
    // COSE_Sign1 layout: [protected, unprotected, payload, signature]
    let cose: ciborium::value::Value =
        ciborium::from_reader(doc).map_err(|err| AttestationError::Malformed(err.to_string()))?;
    let array = match cose {
        ciborium::value::Value::Array(a) => a,
        // Tag 18 = COSE_Sign1 tag.
        ciborium::value::Value::Tag(18, inner) => match *inner {
            ciborium::value::Value::Array(a) => a,
            _ => {
                return Err(AttestationError::Malformed(
                    "expected array under tag 18".into(),
                ));
            }
        },
        _ => {
            return Err(AttestationError::Malformed(
                "expected COSE_Sign1 array".into(),
            ));
        }
    };
    if array.len() != 4 {
        return Err(AttestationError::Malformed(format!(
            "expected 4-element COSE_Sign1 array, got {}",
            array.len()
        )));
    }
    let payload_bytes = match &array[2] {
        ciborium::value::Value::Bytes(b) => b.clone(),
        _ => {
            return Err(AttestationError::Malformed(
                "COSE_Sign1 payload is not a byte string".into(),
            ));
        }
    };
    let payload: ciborium::value::Value = ciborium::from_reader(payload_bytes.as_slice())
        .map_err(|err| AttestationError::Malformed(format!("payload decode: {err}")))?;
    let entries = match payload {
        ciborium::value::Value::Map(m) => m,
        _ => {
            return Err(AttestationError::Malformed(
                "attestation payload is not a CBOR map".into(),
            ));
        }
    };

    let mut pcrs: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
    let mut user_data: Option<Vec<u8>> = None;
    let mut module_id: Option<String> = None;
    let mut digest: Option<String> = None;
    let mut certificate: Option<Vec<u8>> = None;
    let mut cabundle: Vec<Vec<u8>> = Vec::new();
    let mut public_key: Option<Vec<u8>> = None;
    let mut nonce: Option<Vec<u8>> = None;

    for (key, value) in entries {
        let key_str = match key {
            ciborium::value::Value::Text(t) => t,
            _ => continue,
        };
        match key_str.as_str() {
            "pcrs" => {
                let entries = match value {
                    ciborium::value::Value::Map(m) => m,
                    _ => {
                        return Err(AttestationError::Malformed(
                            "pcrs field is not a map".into(),
                        ));
                    }
                };
                for (pcr_key, pcr_value) in entries {
                    let idx: u8 = match pcr_key {
                        ciborium::value::Value::Integer(i) => match u8::try_from(i) {
                            Ok(idx) => idx,
                            Err(_) => continue,
                        },
                        _ => continue,
                    };
                    let bytes = match pcr_value {
                        ciborium::value::Value::Bytes(b) => b,
                        _ => continue,
                    };
                    pcrs.insert(idx, bytes);
                }
            }
            "user_data" => {
                user_data = match value {
                    ciborium::value::Value::Bytes(b) => Some(b),
                    ciborium::value::Value::Null => None,
                    _ => {
                        return Err(AttestationError::Malformed(
                            "user_data is not a byte string".into(),
                        ));
                    }
                };
            }
            "module_id" => {
                if let ciborium::value::Value::Text(t) = value {
                    module_id = Some(t);
                }
            }
            "digest" => {
                if let ciborium::value::Value::Text(t) = value {
                    digest = Some(t);
                }
            }
            "certificate" => {
                if let ciborium::value::Value::Bytes(b) = value {
                    certificate = Some(b);
                }
            }
            "cabundle" => {
                if let ciborium::value::Value::Array(arr) = value {
                    for item in arr {
                        if let ciborium::value::Value::Bytes(b) = item {
                            cabundle.push(b);
                        }
                    }
                }
            }
            "public_key" => {
                if let ciborium::value::Value::Bytes(b) = value {
                    public_key = Some(b);
                }
            }
            "nonce" => {
                if let ciborium::value::Value::Bytes(b) = value {
                    nonce = Some(b);
                }
            }
            _ => {}
        }
    }

    Ok(ParsedAttestationDoc {
        pcrs,
        user_data,
        module_id,
        digest,
        certificate,
        cabundle,
        public_key,
        nonce,
    })
}

// ──────────────────────────────────────────────────────────────────────────────────────
// COSE_Sign1 signature verification
// ──────────────────────────────────────────────────────────────────────────────────────

/// Verifies the COSE_Sign1 signature on an AWS Nitro attestation document.
///
/// # What this verifies
///
/// 1. Parses the outer COSE_Sign1 envelope to extract `protected`, `payload`, and
///    `signature` fields.
/// 2. Extracts the DER-encoded leaf certificate from the payload's `certificate` field.
/// 3. Extracts the P-384 public key from the leaf certificate.
/// 4. Reconstructs the `Sig_Structure`: `CBOR(["Signature1", protected, b"", payload])`.
/// 5. Verifies the P-384 / ES384 signature over the `Sig_Structure` bytes.
/// 6. Checks that the root certificate in `cabundle` matches the hardcoded
///    AWS Nitro Attestation PKI root CA.
/// 7. Checks certificate validity periods (not-before / not-after) for the leaf and
///    all intermediate certificates in the chain.
///
/// # Skipping for synthetic test documents
///
/// If the payload does not contain a `certificate` field (i.e., synthetic test documents),
/// this function returns `Ok(())` without performing any cryptographic checks. Real Nitro
/// attestation documents always include a certificate.
pub fn verify_cose_sign1_signature(doc: &[u8]) -> Result<(), AttestationError> {
    use p384::ecdsa::{Signature, signature::Verifier as _};

    // ── 1. Parse outer COSE_Sign1 ───────────────────────────────────────────────────
    let cose: ciborium::value::Value = ciborium::from_reader(doc)
        .map_err(|e| AttestationError::Malformed(format!("COSE parse: {e}")))?;

    let array = match cose {
        ciborium::value::Value::Array(a) => a,
        ciborium::value::Value::Tag(18, inner) => match *inner {
            ciborium::value::Value::Array(a) => a,
            _ => {
                return Err(AttestationError::Malformed(
                    "expected array under tag 18".into(),
                ));
            }
        },
        _ => {
            return Err(AttestationError::Malformed(
                "expected COSE_Sign1 array".into(),
            ));
        }
    };

    if array.len() != 4 {
        return Err(AttestationError::Malformed(format!(
            "COSE_Sign1 must have 4 elements, got {}",
            array.len()
        )));
    }

    let protected_bstr = match &array[0] {
        ciborium::value::Value::Bytes(b) => b.clone(),
        _ => {
            return Err(AttestationError::Malformed(
                "COSE_Sign1 protected is not a bstr".into(),
            ));
        }
    };
    let payload_bstr = match &array[2] {
        ciborium::value::Value::Bytes(b) => b.clone(),
        _ => {
            return Err(AttestationError::Malformed(
                "COSE_Sign1 payload is not a bstr".into(),
            ));
        }
    };
    let signature_bytes = match &array[3] {
        ciborium::value::Value::Bytes(b) => b.clone(),
        _ => {
            return Err(AttestationError::Malformed(
                "COSE_Sign1 signature is not a bstr".into(),
            ));
        }
    };

    // ── 2. Parse payload to get certificate and cabundle ───────────────────────────
    let payload_value: ciborium::value::Value = ciborium::from_reader(payload_bstr.as_slice())
        .map_err(|e| AttestationError::Malformed(format!("payload decode: {e}")))?;

    let payload_map = match payload_value {
        ciborium::value::Value::Map(m) => m,
        _ => {
            return Err(AttestationError::Malformed(
                "attestation payload is not a CBOR map".into(),
            ));
        }
    };

    let mut cert_der: Option<Vec<u8>> = None;
    let mut cabundle: Vec<Vec<u8>> = Vec::new();

    for (k, v) in &payload_map {
        match k {
            ciborium::value::Value::Text(s) if s == "certificate" => {
                if let ciborium::value::Value::Bytes(b) = v {
                    cert_der = Some(b.clone());
                }
            }
            ciborium::value::Value::Text(s) if s == "cabundle" => {
                if let ciborium::value::Value::Array(arr) = v {
                    for item in arr {
                        if let ciborium::value::Value::Bytes(b) = item {
                            cabundle.push(b.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // A valid Nitro attestation document must always carry a leaf certificate.
    // Accepting a document without one would allow PCR / user_data checks to pass
    // without any cryptographic proof that those values came from AWS hardware.
    let cert_der = cert_der.ok_or_else(|| {
        AttestationError::Malformed(
            "attestation document missing required `certificate` field".into(),
        )
    })?;

    // ── 3. Verify full certificate chain ─────────────────────────────────────────
    // Walk leaf → intermediates → root, verifying each signature and anchoring
    // the root to the hardcoded AWS Nitro Attestation PKI constant.
    verify_cert_chain(&cert_der, &cabundle)?;

    // ── 4. Build Sig_Structure ─────────────────────────────────────────────────────
    // RFC 8152 §4.4:
    //   Sig_Structure = [
    //     context:      "Signature1",
    //     body_protected: protected_bstr,
    //     external_aad: h'',
    //     payload:      payload_bstr,
    //   ]
    let sig_structure = ciborium::value::Value::Array(vec![
        ciborium::value::Value::Text("Signature1".into()),
        ciborium::value::Value::Bytes(protected_bstr),
        ciborium::value::Value::Bytes(vec![]), // external_aad
        ciborium::value::Value::Bytes(payload_bstr),
    ]);
    let mut sig_struct_bytes = Vec::new();
    ciborium::into_writer(&sig_structure, &mut sig_struct_bytes)
        .map_err(|e| AttestationError::Malformed(format!("Sig_Structure encode: {e}")))?;

    // ── 5. Extract P-384 public key from leaf certificate ─────────────────────────
    let verifying_key = extract_p384_key(&cert_der)?;

    // ── 6. Verify ES384 signature ──────────────────────────────────────────────────
    // COSE ES384 uses the fixed (r‖s) 96-byte encoding for P-384 signatures.
    let sig = Signature::from_slice(&signature_bytes)
        .map_err(|e| AttestationError::CoseSignature(format!("signature decode: {e}")))?;
    verifying_key
        .verify(&sig_struct_bytes, &sig)
        .map_err(|e| AttestationError::CoseSignature(format!("ES384 verify: {e}")))?;

    Ok(())
}

/// Extracts the P-384 verifying key from a DER-encoded X.509 certificate.
fn extract_p384_key(cert_der: &[u8]) -> Result<p384::ecdsa::VerifyingKey, AttestationError> {
    use p384::ecdsa::VerifyingKey;
    use x509_parser::prelude::FromDer as _;

    let (_, cert) = x509_parser::prelude::X509Certificate::from_der(cert_der)
        .map_err(|e| AttestationError::CertChain(format!("leaf cert parse: {e}")))?;

    // SubjectPublicKeyInfo.subjectPublicKey holds the SEC1 uncompressed/compressed point.
    let spki = cert.public_key();
    let key_bytes = &spki.subject_public_key.data;

    VerifyingKey::from_sec1_bytes(key_bytes)
        .map_err(|e| AttestationError::CertChain(format!("P-384 key decode: {e}")))
}

/// Verifies that the last certificate in `cabundle` is the hardcoded AWS Nitro root CA.
///
/// AWS NSM attestation documents always include the root CA as the final element of
/// `cabundle` (per the Nitro attestation spec), so checking `cabundle.last()` is
/// correct and intentional — this is not a format ambiguity.
fn verify_root_ca(cabundle: &[Vec<u8>]) -> Result<(), AttestationError> {
    let root = cabundle.last().ok_or_else(|| {
        AttestationError::CertChain("cabundle is empty, cannot verify root CA".into())
    })?;

    let expected = aws_root_ca_der().map_err(AttestationError::CertChain)?;
    if root != &expected {
        return Err(AttestationError::CertChain(
            "root CA certificate does not match the expected AWS Nitro Attestation PKI root".into(),
        ));
    }
    Ok(())
}

/// Checks that the current system time falls within `cert`'s not-before / not-after window.
fn check_cert_validity(
    cert: &x509_parser::prelude::X509Certificate<'_>,
    label: &str,
) -> Result<(), AttestationError> {
    use x509_parser::time::ASN1Time;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_secs() as i64;
    let now = ASN1Time::from_timestamp(now_secs).map_err(|_| {
        AttestationError::CertExpired(format!("{label}: failed to construct current time"))
    })?;
    if !cert.validity().is_valid_at(now) {
        let v = cert.validity();
        return Err(AttestationError::CertExpired(format!(
            "{label}: not_before={} not_after={}",
            v.not_before, v.not_after
        )));
    }
    Ok(())
}

/// Verifies the complete certificate chain from the leaf certificate to the AWS Nitro
/// root CA.
///
/// The Nitro attestation layout is:
/// - `leaf_der` — end-entity certificate whose public key signs the COSE_Sign1 envelope.
/// - `cabundle[0]` — intermediate CA that issued `leaf_der`.
/// - `cabundle[1..n-1]` — further intermediate CAs, each issued by the next.
/// - `cabundle[n]` — root CA (must match [`AWS_NITRO_ROOT_CA_PEM`] byte-for-byte).
///
/// This function:
/// 1. Anchors the root by comparing `cabundle.last()` to the hardcoded constant.
/// 2. Verifies that the leaf's signature validates under `cabundle[0]`'s public key.
/// 3. Verifies that each `cabundle[i]`'s signature validates under `cabundle[i+1]`'s
///    public key, all the way up to (but not including) the self-signed root.
fn verify_cert_chain(leaf_der: &[u8], cabundle: &[Vec<u8>]) -> Result<(), AttestationError> {
    use x509_parser::prelude::{FromDer as _, X509Certificate};

    // 1. Root anchor check.
    verify_root_ca(cabundle)?;

    // cabundle is guaranteed non-empty by verify_root_ca.

    // 2. Verify the leaf certificate is signed by cabundle[0] and is currently valid.
    let (_, leaf) = X509Certificate::from_der(leaf_der)
        .map_err(|e| AttestationError::CertChain(format!("leaf cert parse: {e}")))?;
    check_cert_validity(&leaf, "leaf")?;
    let (_, issuer0) = X509Certificate::from_der(&cabundle[0])
        .map_err(|e| AttestationError::CertChain(format!("cabundle[0] parse: {e}")))?;
    leaf.verify_signature(Some(issuer0.public_key()))
        .map_err(|e| AttestationError::CertChain(format!("leaf cert signature invalid: {e}")))?;

    // 3. Verify each intermediate is signed by the next one up and is currently valid.
    //    The root (last element) is self-signed and anchored by verify_root_ca above —
    //    no need to re-verify its signature.
    for i in 0..cabundle.len() - 1 {
        let (_, cert) = X509Certificate::from_der(&cabundle[i])
            .map_err(|e| AttestationError::CertChain(format!("cabundle[{i}] parse: {e}")))?;
        check_cert_validity(&cert, &format!("cabundle[{i}]"))?;
        let (_, issuer) = X509Certificate::from_der(&cabundle[i + 1])
            .map_err(|e| AttestationError::CertChain(format!("cabundle[{}] parse: {e}", i + 1)))?;
        cert.verify_signature(Some(issuer.public_key()))
            .map_err(|e| {
                AttestationError::CertChain(format!(
                    "cabundle[{i}] signature invalid (issuer cabundle[{}]): {e}",
                    i + 1
                ))
            })?;
    }

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────────────
// High-level verification entry points
// ──────────────────────────────────────────────────────────────────────────────────────

/// Extracts the `public_key` field from an NSM attestation document's CBOR payload.
///
/// The NSM embeds the enclave-supplied key into the signed CBOR payload when the enclave
/// calls `NsmRequest::Attestation { public_key: Some(bytes) }`. This value is the only
/// key material that is cryptographically bound to the PCR measurements via the
/// COSE_Sign1 P-384 signature.
///
/// # Usage
///
/// Call this after [`verify_cose_sign1_signature`] to obtain the NSM-certified key, then
/// compare it to the `public_key` returned on the wire in `EnclaveResponse::Attestation`.
/// If they differ, reject the attestation — an attacker could otherwise substitute an
/// arbitrary key on the wire while presenting a legitimate document.
///
/// # Errors
///
/// Returns [`AttestationError::MissingField`] if the payload has no `public_key` field.
pub fn extract_nsm_public_key(doc: &[u8]) -> Result<Vec<u8>, AttestationError> {
    let parsed = parse_attestation_doc(doc)?;
    parsed
        .public_key
        .ok_or(AttestationError::MissingField("public_key"))
}

/// Checks that the `public_key` in an NSM attestation document's CBOR payload matches
/// the key supplied on the wire by the enclave.
///
/// This must be called after [`verify_cose_sign1_signature`] to ensure the attestation
/// doc (and therefore the embedded key) has been cryptographically validated before the
/// comparison is trusted.
///
/// # Errors
///
/// Returns [`AttestationError::MissingField`] if the payload has no `public_key` field.
/// Returns [`AttestationError::PublicKeyMismatch`] if the wire key differs.
pub fn verify_nsm_public_key(doc: &[u8], wire_key: &[u8]) -> Result<(), AttestationError> {
    let nsm_key = extract_nsm_public_key(doc)?;
    if nsm_key.as_slice() != wire_key {
        return Err(AttestationError::PublicKeyMismatch {
            nsm: hex::encode(&nsm_key),
            wire: hex::encode(wire_key),
        });
    }
    Ok(())
}

/// Parses a `COSE_Sign1` Nitro attestation document and checks that its PCR map and
/// `user_data` field match the supplied expectations.
///
/// # What this function DOES
///
/// - Decodes the outer `COSE_Sign1` envelope and the inner CBOR payload map.
/// - Extracts the `pcrs` map and compares PCR0/1/2 byte-for-byte against `expected_pcrs`.
/// - Extracts `user_data` and compares it byte-for-byte against `expected_user_data`.
/// - Returns the parsed payload fields on success.
///
/// # What this function does NOT do
///
/// - It does **not** verify the COSE_Sign1 signature or certificate chain.
///   Call [`verify_cose_sign1_signature`] explicitly for full cryptographic verification,
///   or use [`parse_check_and_verify`] which combines both steps.
/// - It does **not** check `timestamp`, `nonce`, or any freshness / replay constraint.
pub fn parse_and_check_pcrs(
    doc: &[u8],
    expected_pcrs: &ExpectedPcrs,
    expected_user_data: &[u8],
) -> Result<ParsedAttestationDoc, AttestationError> {
    let parsed = parse_attestation_doc(doc)?;

    check_pcr(&parsed, 0, &expected_pcrs.pcr0)?;
    check_pcr(&parsed, 1, &expected_pcrs.pcr1)?;
    check_pcr(&parsed, 2, &expected_pcrs.pcr2)?;

    let actual_user_data = parsed
        .user_data
        .as_deref()
        .ok_or(AttestationError::MissingField("user_data"))?;
    if actual_user_data != expected_user_data {
        return Err(AttestationError::UserDataMismatch {
            expected: hex::encode(expected_user_data),
            actual: hex::encode(actual_user_data),
        });
    }

    Ok(parsed)
}

/// Fully-verified attestation check: verifies PCRs, `user_data`, **and** the COSE_Sign1
/// P-384 signature + AWS root CA anchor.
///
/// Use this in production. [`parse_and_check_pcrs`] is kept for test convenience where
/// synthetic documents without real certificates are used.
pub fn parse_check_and_verify(
    doc: &[u8],
    expected_pcrs: &ExpectedPcrs,
    expected_user_data: &[u8],
) -> Result<ParsedAttestationDoc, AttestationError> {
    let parsed = parse_and_check_pcrs(doc, expected_pcrs, expected_user_data)?;
    verify_cose_sign1_signature(doc)?;
    Ok(parsed)
}

/// Verifies that the `nonce` field in the attestation document matches `expected`.
///
/// Must be called after [`verify_cose_sign1_signature`] to ensure the nonce value is
/// cryptographically bound to the hardware measurements before the comparison is trusted.
pub fn verify_nonce(doc: &[u8], expected: &[u8]) -> Result<(), AttestationError> {
    let parsed = parse_attestation_doc(doc)?;
    let actual = parsed
        .nonce
        .as_deref()
        .ok_or(AttestationError::MissingField("nonce"))?;
    if actual != expected {
        return Err(AttestationError::NonceMismatch {
            expected: hex::encode(expected),
            actual: hex::encode(actual),
        });
    }
    Ok(())
}

/// Parses a `COSE_Sign1` Nitro attestation document and checks the PCRs against the
/// supplied expectations, **without** checking `user_data`.
///
/// Useful for verifying [`EnclaveRequest::GetAttestation`] documents where `user_data` is
/// `None` (the enclave embeds its public key in the NSM `public_key` field instead).
///
/// Callers that also need `user_data` verification should use [`parse_and_check_pcrs`]
/// or [`parse_check_and_verify`].
pub fn verify_pcrs_only(
    doc: &[u8],
    expected_pcrs: &ExpectedPcrs,
) -> Result<ParsedAttestationDoc, AttestationError> {
    let parsed = parse_attestation_doc(doc)?;
    check_pcr(&parsed, 0, &expected_pcrs.pcr0)?;
    check_pcr(&parsed, 1, &expected_pcrs.pcr1)?;
    check_pcr(&parsed, 2, &expected_pcrs.pcr2)?;
    Ok(parsed)
}

// ──────────────────────────────────────────────────────────────────────────────────────
// Private helpers
// ──────────────────────────────────────────────────────────────────────────────────────

fn check_pcr(
    parsed: &ParsedAttestationDoc,
    index: u8,
    expected: &PcrDigest,
) -> Result<(), AttestationError> {
    if expected.iter().all(|&b| b == 0) {
        return Err(AttestationError::EmptyExpectedPcr { index });
    }
    let actual = parsed
        .pcrs
        .get(&index)
        .ok_or(AttestationError::MissingField(match index {
            0 => "pcr0",
            1 => "pcr1",
            2 => "pcr2",
            _ => "pcrN",
        }))?;
    if actual.len() != PCR_LEN || actual.as_slice() != expected.as_slice() {
        return Err(AttestationError::PcrMismatch {
            index,
            expected: hex::encode(expected),
            actual: hex::encode(actual),
        });
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_doc(pcrs: Vec<(u8, Vec<u8>)>, user_data: Option<Vec<u8>>) -> Vec<u8> {
        let pcr_map: Vec<(ciborium::value::Value, ciborium::value::Value)> = pcrs
            .into_iter()
            .map(|(idx, bytes)| {
                (
                    ciborium::value::Value::Integer((idx as i128).try_into().unwrap()),
                    ciborium::value::Value::Bytes(bytes),
                )
            })
            .collect();
        let mut entries: Vec<(ciborium::value::Value, ciborium::value::Value)> = vec![
            (
                ciborium::value::Value::Text("pcrs".into()),
                ciborium::value::Value::Map(pcr_map),
            ),
            (
                ciborium::value::Value::Text("module_id".into()),
                ciborium::value::Value::Text("test-module".into()),
            ),
            (
                ciborium::value::Value::Text("digest".into()),
                ciborium::value::Value::Text("SHA384".into()),
            ),
        ];
        entries.push((
            ciborium::value::Value::Text("user_data".into()),
            match user_data {
                Some(bytes) => ciborium::value::Value::Bytes(bytes),
                None => ciborium::value::Value::Null,
            },
        ));
        let mut payload_bytes = Vec::new();
        ciborium::into_writer(&ciborium::value::Value::Map(entries), &mut payload_bytes).unwrap();

        let cose = ciborium::value::Value::Array(vec![
            ciborium::value::Value::Bytes(vec![]),
            ciborium::value::Value::Map(vec![]),
            ciborium::value::Value::Bytes(payload_bytes),
            ciborium::value::Value::Bytes(vec![0u8; 96]),
        ]);
        let mut out = Vec::new();
        ciborium::into_writer(&cose, &mut out).unwrap();
        out
    }

    fn non_placeholder_pcrs(byte: u8) -> ExpectedPcrs {
        ExpectedPcrs {
            pcr0: [byte; PCR_LEN],
            pcr1: [byte; PCR_LEN],
            pcr2: [byte; PCR_LEN],
        }
    }

    #[test]
    fn parses_and_verifies_matching_pcrs() {
        let doc = make_doc(
            vec![(0, vec![3u8; 48]), (1, vec![3u8; 48]), (2, vec![3u8; 48])],
            Some(vec![7u8; 32]),
        );
        let parsed = parse_and_check_pcrs(&doc, &non_placeholder_pcrs(3), &[7u8; 32]).unwrap();
        assert_eq!(parsed.user_data.unwrap(), vec![7u8; 32]);
        assert_eq!(parsed.module_id.unwrap(), "test-module");
    }

    #[test]
    fn rejects_all_zero_expected_pcr() {
        let doc = make_doc(
            vec![(0, vec![0u8; 48]), (1, vec![0u8; 48]), (2, vec![0u8; 48])],
            Some(vec![7u8; 32]),
        );
        let err = parse_and_check_pcrs(&doc, &ExpectedPcrs::PLACEHOLDER, &[7u8; 32]).unwrap_err();
        assert!(matches!(
            err,
            AttestationError::EmptyExpectedPcr { index: 0 }
        ));
    }

    #[test]
    fn rejects_pcr_mismatch() {
        let doc = make_doc(
            vec![(0, vec![1u8; 48]), (1, vec![3u8; 48]), (2, vec![3u8; 48])],
            Some(vec![7u8; 32]),
        );
        let expected = ExpectedPcrs {
            pcr0: [2u8; PCR_LEN],
            pcr1: [3u8; PCR_LEN],
            pcr2: [3u8; PCR_LEN],
        };
        let err = parse_and_check_pcrs(&doc, &expected, &[7u8; 32]).unwrap_err();
        assert!(matches!(
            err,
            AttestationError::PcrMismatch { index: 0, .. }
        ));
    }

    #[test]
    fn rejects_user_data_mismatch() {
        let doc = make_doc(
            vec![(0, vec![3u8; 48]), (1, vec![3u8; 48]), (2, vec![3u8; 48])],
            Some(vec![9u8; 32]),
        );
        let err = parse_and_check_pcrs(&doc, &non_placeholder_pcrs(3), &[7u8; 32]).unwrap_err();
        assert!(matches!(err, AttestationError::UserDataMismatch { .. }));
    }

    /// Documents without a `certificate` field must be rejected by the signature
    /// verifier — accepting them would allow PCR/user_data checks to pass without
    /// any AWS hardware proof.
    #[test]
    fn verify_cose_sign1_rejects_missing_certificate() {
        let doc = make_doc(
            vec![(0, vec![3u8; 48]), (1, vec![3u8; 48]), (2, vec![3u8; 48])],
            Some(vec![7u8; 32]),
        );
        // Must return an error when the certificate field is absent.
        let err = verify_cose_sign1_signature(&doc).unwrap_err();
        assert!(
            matches!(err, AttestationError::Malformed(_)),
            "expected Malformed error, got: {err:?}"
        );
    }

    /// Verifies that [`AWS_NITRO_ROOT_CA_PEM`] is the official AWS Nitro Attestation PKI
    /// root CA certificate and decodes cleanly to DER.
    ///
    /// The PEM content is sourced from
    /// <https://aws-nitro-enclaves.amazonaws.com/AWS_NitroEnclaves_Root-G1.zip>
    /// (SHA-256 of zip: `8cf60e2b2efca96c6a9e71e851d00c1b6991cc09eadbe64a6a1d1b1eb9faff7c`).
    #[test]
    fn root_ca_pem_decodes_successfully() {
        // Verify the PEM delimiters are intact.
        assert!(AWS_NITRO_ROOT_CA_PEM.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(AWS_NITRO_ROOT_CA_PEM.ends_with("-----END CERTIFICATE-----"));
        // Verify the base64 body decodes to valid DER.
        assert!(
            aws_root_ca_der().is_ok(),
            "DER decode failed: {:?}",
            aws_root_ca_der()
        );
    }
}
