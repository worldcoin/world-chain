//! Host-side verification of NSM attestation documents.
//!
//! A Nitro NSM attestation document is a COSE_Sign1 structure whose payload is a CBOR map
//! containing the PCRs, the optional `user_data` field, the enclave certificate, and the
//! certificate chain back to the AWS Nitro Attestation PKI root. This module checks:
//!
//! 1. PCR / `user_data` invariants needed by the World fault proof flow.
//! 2. The COSE_Sign1 P-384 signature against the leaf certificate's public key.
//! 3. An RFC 5280 certification path from the leaf to the AWS Nitro Root CA.
//! 4. The keyUsage bits AWS requires, which `webpki` does not inspect.
//! 5. Nonce freshness: the host supplies a per-request nonce that the NSM embeds in the
//!    signed payload, preventing replay of captured attestation documents.

use crate::{ExpectedPcrs, PCR_LEN, PcrDigest};
use p384::ecdsa::{Signature, signature::Verifier as _};
use rustls_pki_types::{CertificateDer, SignatureVerificationAlgorithm, UnixTime};
use std::collections::BTreeMap;
use webpki::{EndEntityCert, ExtendedKeyUsageValidator, KeyPurposeIdIter};

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
    /// DER-encoded CA bundle, root first. The last element issued `certificate`.
    pub cabundle: Vec<Vec<u8>>,
    /// Optional `public_key` field. Present when the enclave called `NsmRequest::Attestation`
    /// with `public_key: Some(bytes)` — i.e., for [`EnclaveRequest::PublicKey`] responses.
    /// The bytes are the uncompressed SEC1 encoding (`0x04 || X || Y`, 65 bytes) of the
    /// enclave's ephemeral secp256k1 key.
    ///
    /// Use [`extract_nsm_public_key`] to obtain this value with a mandatory-presence check.
    pub public_key: Option<Vec<u8>>,
    /// Optional `nonce` field. Present when the host supplied a nonce in the request, which
    /// the NSM embeds verbatim into the signed payload for replay protection.
    pub nonce: Option<Vec<u8>>,
}

/// Parses a `COSE_Sign1` Nitro attestation document and returns the inner payload fields the
/// host cares about.
///
/// This does **not** verify the signature or certificate chain — call
/// [`verify_cose_sign1_signature`] for full cryptographic verification.
pub fn parse_attestation_doc(doc: &[u8]) -> Result<ParsedAttestationDoc, AttestationError> {
    let sign1 = crate::cose::parse_cose_sign1(doc)
        .map_err(|err| AttestationError::Malformed(err.to_string()))?;
    // `parse_cose_sign1` rejects an absent payload, so this is always `Some`.
    let payload_bytes = sign1
        .payload
        .ok_or_else(|| AttestationError::Malformed("COSE_Sign1 payload is absent".into()))?;
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

/// Verifies the COSE_Sign1 signature on an AWS Nitro attestation document.
///
/// # What this verifies
///
/// 1. Decodes the COSE_Sign1 envelope via [`crate::cose::parse_cose_sign1`].
/// 2. Validates an RFC 5280 path from the payload's leaf certificate to the pinned AWS Nitro
///    root CA, plus the keyUsage bits AWS requires.
/// 3. Verifies the ES384 signature over the `Sig_Structure`, using the leaf certificate's key.
///
/// A document without a `certificate` field is rejected: accepting one would let PCR and
/// `user_data` checks pass with no cryptographic proof the values came from AWS hardware.
pub fn verify_cose_sign1_signature(doc: &[u8]) -> Result<(), AttestationError> {
    let sign1 = crate::cose::parse_cose_sign1(doc)
        .map_err(|e| AttestationError::Malformed(format!("COSE parse: {e}")))?;
    // `parse_cose_sign1` rejects an absent payload, so this is always `Some`.
    let payload_bstr = sign1
        .payload
        .as_deref()
        .ok_or_else(|| AttestationError::Malformed("COSE_Sign1 payload is absent".into()))?;

    let payload_value: ciborium::value::Value = ciborium::from_reader(payload_bstr)
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

    // Walk leaf → intermediates → root, verifying each signature and anchoring
    // the root to the hardcoded AWS Nitro Attestation PKI constant.
    verify_cert_chain(&cert_der, &cabundle)?;

    let verifying_key = extract_p384_key(&cert_der)?;

    // `coset` rebuilds the RFC 8152 §4.4 `Sig_Structure` from the original `protected` bytes.
    // COSE ES384 uses the fixed (r‖s) 96-byte encoding for P-384 signatures.
    sign1.verify_signature(&[], |signature_bytes, sig_struct_bytes| {
        let sig = Signature::from_slice(signature_bytes)
            .map_err(|e| AttestationError::CoseSignature(format!("signature decode: {e}")))?;
        verifying_key
            .verify(sig_struct_bytes, &sig)
            .map_err(|e| AttestationError::CoseSignature(format!("ES384 verify: {e}")))
    })
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

/// Extracts the leaf certificate's P-384 public key as the uncompressed `x || y`
/// coordinate pair (96 bytes, without the SEC1 `0x04` prefix).
///
/// This is the form the [`crate::p384_hints::collect_hints`] generator expects for the
/// `leaf_pubkey` argument when producing hints for the final attestation signature. The
/// leaf certificate is the one that signs the COSE_Sign1 document, so its public key is
/// what verifies the attestation signature.
///
/// # Errors
///
/// Returns [`AttestationError::Malformed`] if the document carries no leaf certificate,
/// and [`AttestationError::CertChain`] if the certificate or its key cannot be parsed.
pub fn leaf_cert_pubkey_xy(doc: &[u8]) -> Result<[u8; 96], AttestationError> {
    let parsed = parse_attestation_doc(doc)?;
    let cert_der = parsed.certificate.ok_or_else(|| {
        AttestationError::Malformed(
            "attestation document missing required `certificate` field".into(),
        )
    })?;

    cert_pubkey_xy(&cert_der)
}

/// Extracts any certificate's P-384 public key as the uncompressed `x || y` coordinate pair
/// (96 bytes, without the SEC1 `0x04` prefix).
pub fn cert_pubkey_xy(cert_der: &[u8]) -> Result<[u8; 96], AttestationError> {
    let verifying_key = extract_p384_key(cert_der)?;
    let point = verifying_key.to_encoded_point(false);
    let bytes = point.as_bytes();

    // Uncompressed SEC1 encoding is `0x04 || X (48) || Y (48)` = 97 bytes.
    if bytes.len() != 97 || bytes[0] != 0x04 {
        return Err(AttestationError::CertChain(format!(
            "unexpected public key encoding ({} bytes, prefix 0x{:02x})",
            bytes.len(),
            bytes.first().copied().unwrap_or(0)
        )));
    }

    let mut out = [0u8; 96];
    out.copy_from_slice(&bytes[1..]);
    Ok(out)
}

/// Verifies that the _first_ certificate in `cabundle` is `expected_root`, which callers set to
/// the pinned AWS Nitro root CA. AWS orders `cabundle` root-first.
///
/// See: [AWS Nitro Enclaves NSM API — attestation process](https://github.com/aws/aws-nitro-enclaves-nsm-api/blob/main/docs/attestation_process.md)
fn verify_root_ca_against(
    cabundle: &[Vec<u8>],
    expected_root: &[u8],
) -> Result<(), AttestationError> {
    let root = cabundle.first().ok_or_else(|| {
        AttestationError::CertChain("cabundle is empty, cannot verify root CA".into())
    })?;
    if root.as_slice() != expected_root {
        return Err(AttestationError::CertChain(
            "root CA certificate does not match the expected AWS Nitro Attestation PKI root".into(),
        ));
    }
    Ok(())
}

/// Checks the pinned root's own validity window.
fn check_root_validity(root_der: &[u8], now_secs: i64) -> Result<(), AttestationError> {
    use x509_parser::prelude::{ASN1Time, FromDer as _, X509Certificate};

    let (_, root) = X509Certificate::from_der(root_der)
        .map_err(|e| AttestationError::CertChain(format!("pinned root CA parse: {e}")))?;

    let now = ASN1Time::from_timestamp(now_secs).map_err(|e| {
        AttestationError::CertExpired(format!("current time is unrepresentable: {e}"))
    })?;

    if !root.validity().is_valid_at(now) {
        return Err(AttestationError::CertExpired(format!(
            "pinned root CA outside its validity window (notBefore {}, notAfter {}, now {now_secs})",
            root.validity().not_before.timestamp(),
            root.validity().not_after.timestamp(),
        )));
    }
    Ok(())
}

/// AWS signs every Nitro certificate with ecdsa-with-SHA384; naming only that pins the chain
/// away from weaker primitives.
static NITRO_SIG_ALGS: &[&dyn SignatureVerificationAlgorithm] = &[webpki::ring::ECDSA_P384_SHA384];

/// Nitro certificates carry no EKU, so no policy applies. `webpki` requires one to be supplied.
struct AnyExtendedKeyUsage;

impl ExtendedKeyUsageValidator for AnyExtendedKeyUsage {
    fn validate(&self, _eku: KeyPurposeIdIter<'_, '_>) -> Result<(), webpki::Error> {
        Ok(())
    }
}

/// Keeps expiry distinguishable from the rest, so a stale document reads differently from a
/// forged one. `Debug` names the `webpki::Error` variant; `Display` collapses several into one.
fn map_path_error(err: webpki::Error) -> AttestationError {
    match err {
        webpki::Error::CertExpired { .. } | webpki::Error::CertNotValidYet { .. } => {
            AttestationError::CertExpired(format!("{err:?}"))
        }
        other => AttestationError::CertChain(format!("path validation failed: {other:?}")),
    }
}

/// Verifies the chain from `leaf_der` up to the AWS Nitro root CA.
///
/// See: [AWS Nitro Enclaves NSM API — attestation process](https://github.com/aws/aws-nitro-enclaves-nsm-api/blob/main/docs/attestation_process.md)
fn verify_cert_chain(leaf_der: &[u8], cabundle: &[Vec<u8>]) -> Result<(), AttestationError> {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_secs() as i64;
    verify_cert_chain_at(leaf_der, cabundle, now_secs)
}

/// `verify_cert_chain` with the clock injected, so tests can pin a real AWS chain to a time
/// inside its validity window.
fn verify_cert_chain_at(
    leaf_der: &[u8],
    cabundle: &[Vec<u8>],
    now_secs: i64,
) -> Result<(), AttestationError> {
    let expected_root = aws_root_ca_der().map_err(AttestationError::CertChain)?;
    verify_cert_chain_against(leaf_der, cabundle, &expected_root, now_secs)
}

/// `verify_cert_chain_at` with the trust anchor injected too. Tests use this to run generated
/// chains through the real walk, including the cases AWS never issues.
fn verify_cert_chain_against(
    leaf_der: &[u8],
    cabundle: &[Vec<u8>],
    expected_root: &[u8],
    now_secs: i64,
) -> Result<(), AttestationError> {
    // Shape check only: the anchor below comes from `expected_root`, never from the document.
    verify_root_ca_against(cabundle, expected_root)?;
    check_root_validity(expected_root, now_secs)?;

    let root = CertificateDer::from(expected_root);
    let anchor = webpki::anchor_from_trusted_cert(&root)
        .map_err(|e| AttestationError::CertChain(format!("pinned root CA parse: {e:?}")))?;

    // `cabundle[0]` is the root, passed as the anchor. Order of the rest is not load-bearing.
    let intermediates: Vec<CertificateDer<'_>> = cabundle[1..]
        .iter()
        .map(|der| CertificateDer::from(der.as_slice()))
        .collect();

    let leaf_der_typed = CertificateDer::from(leaf_der);
    let leaf = EndEntityCert::try_from(&leaf_der_typed)
        .map_err(|e| AttestationError::CertChain(format!("leaf cert parse: {e:?}")))?;

    let now = UnixTime::since_unix_epoch(std::time::Duration::from_secs(
        u64::try_from(now_secs).map_err(|_| {
            AttestationError::CertExpired("current time is before the Unix epoch".into())
        })?,
    ));

    // Revocation is `None`: AWS publishes no CRL or OCSP for the Nitro Attestation PKI.
    leaf.verify_for_usage(
        NITRO_SIG_ALGS,
        &[anchor],
        &intermediates,
        now,
        AnyExtendedKeyUsage,
        None,
        None,
    )
    .map_err(map_path_error)?;

    check_key_usage_policy(leaf_der, cabundle)
}

/// The keyUsage bits AWS requires ([§3.2.3.3]) and `webpki` never enforces.
///
/// [§3.2.3.3]: https://github.com/aws/aws-nitro-enclaves-nsm-api/blob/main/docs/attestation_process.md
fn check_key_usage_policy(leaf_der: &[u8], cabundle: &[Vec<u8>]) -> Result<(), AttestationError> {
    use x509_parser::prelude::{FromDer as _, X509Certificate};

    for (i, der) in cabundle.iter().enumerate() {
        let (_, cert) = X509Certificate::from_der(der)
            .map_err(|e| AttestationError::CertChain(format!("cabundle[{i}] parse: {e}")))?;
        check_ca_key_usage(&cert, &format!("cabundle[{i}]"))?;
    }

    let (_, leaf) = X509Certificate::from_der(leaf_der)
        .map_err(|e| AttestationError::CertChain(format!("leaf cert parse: {e}")))?;
    check_end_entity_policy(&leaf, "leaf")
}

/// Requires `keyCertSign` on a CA, and the extension itself — a CA with none would pass.
fn check_ca_key_usage(
    cert: &x509_parser::prelude::X509Certificate<'_>,
    label: &str,
) -> Result<(), AttestationError> {
    let ku = cert
        .key_usage()
        .map_err(|e| AttestationError::CertChain(format!("{label}: keyUsage parse: {e}")))?
        .ok_or_else(|| AttestationError::CertChain(format!("{label}: missing keyUsage")))?;
    if !ku.value.key_cert_sign() {
        return Err(AttestationError::CertChain(format!(
            "{label}: keyUsage does not permit keyCertSign"
        )));
    }
    Ok(())
}

/// Requires `digitalSignature` on the leaf and no `pathLenConstraint`, which belongs on a CA.
///
/// Criticality is deliberately not required: AWS marks `keyUsage` critical on the CAs but not
/// on the leaf. `webpki` tolerates `pathLenConstraint` here, so that check stays local.
fn check_end_entity_policy(
    cert: &x509_parser::prelude::X509Certificate<'_>,
    label: &str,
) -> Result<(), AttestationError> {
    if let Ok(Some(bc)) = cert.basic_constraints()
        && bc.value.path_len_constraint.is_some()
    {
        return Err(AttestationError::CertChain(format!(
            "{label}: end-entity cert carries a pathLenConstraint"
        )));
    }

    let ku = cert
        .key_usage()
        .map_err(|e| AttestationError::CertChain(format!("{label}: keyUsage parse: {e}")))?
        .ok_or_else(|| AttestationError::CertChain(format!("{label}: missing keyUsage")))?;
    if !ku.value.digital_signature() {
        return Err(AttestationError::CertChain(format!(
            "{label}: keyUsage does not assert digitalSignature"
        )));
    }
    Ok(())
}

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
/// Useful for verifying [`EnclaveRequest::PublicKey`] documents where `user_data` is
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

    /// Anchors against the pinned AWS root, the same value production passes in.
    fn verify_root_ca(cabundle: &[Vec<u8>]) -> Result<(), AttestationError> {
        let expected = aws_root_ca_der().unwrap();
        verify_root_ca_against(cabundle, &expected)
    }

    #[test]
    fn anchors_root_at_first_cabundle_element() {
        let root = aws_root_ca_der().unwrap();
        let intermediate = vec![0xAAu8; 64];

        verify_root_ca(&[root.clone(), intermediate.clone()]).unwrap();
        verify_root_ca(std::slice::from_ref(&root)).unwrap();

        let err = verify_root_ca(&[intermediate, root]).unwrap_err();
        assert!(
            matches!(err, AttestationError::CertChain(_)),
            "got: {err:?}"
        );
    }

    #[test]
    fn rejects_cabundle_without_the_pinned_root() {
        let err = verify_root_ca(&[vec![0xAAu8; 64], vec![0xBBu8; 64]]).unwrap_err();
        assert!(
            matches!(err, AttestationError::CertChain(_)),
            "got: {err:?}"
        );
    }

    #[test]
    fn rejects_empty_cabundle() {
        let err = verify_root_ca(&[]).unwrap_err();
        match err {
            AttestationError::CertChain(msg) => assert!(msg.contains("empty"), "got: {msg}"),
            other => panic!("expected CertChain, got: {other:?}"),
        }
    }

    // Real chain captured from a us-east-1 Nitro enclave on 2026-08-03. Public certificates,
    // no secrets. The clock is pinned because AWS leaf certs live ~3 hours.
    const AWS_ROOT: &[u8] = include_bytes!("testdata/aws_root.der");
    const AWS_REGIONAL: &[u8] = include_bytes!("testdata/aws_regional.der");
    const AWS_ZONAL: &[u8] = include_bytes!("testdata/aws_zonal.der");
    const AWS_INSTANCE: &[u8] = include_bytes!("testdata/aws_instance.der");
    const AWS_LEAF: &[u8] = include_bytes!("testdata/aws_leaf.der");
    /// Midpoint of the window in which all five fixture certs are simultaneously valid.
    const AWS_CHAIN_VALID_AT: i64 = 1_785_790_194;

    fn aws_cabundle() -> Vec<Vec<u8>> {
        vec![
            AWS_ROOT.to_vec(),
            AWS_REGIONAL.to_vec(),
            AWS_ZONAL.to_vec(),
            AWS_INSTANCE.to_vec(),
        ]
    }

    // AWS only ever issues well-formed chains, so the fixtures above cannot exercise a
    // malformed one. These build chains with rcgen instead: no expiry to work around, and
    // the shape is ours to break.

    use rcgen::{
        BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose,
        PKCS_ECDSA_P384_SHA384,
    };

    struct GenCert {
        der: Vec<u8>,
        key: KeyPair,
        params: CertificateParams,
    }

    /// One link in a generated chain. `ca` of `None` means an end-entity certificate.
    fn gen_cert(cn: &str, ca: Option<BasicConstraints>, issuer: Option<&GenCert>) -> GenCert {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.distinguished_name.push(DnType::CommonName, cn);
        match ca {
            Some(bc) => {
                params.is_ca = IsCa::Ca(bc);
                params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
            }
            None => {
                params.is_ca = IsCa::ExplicitNoCa;
                params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
            }
        }
        let der = match issuer {
            None => params.self_signed(&key).unwrap().der().to_vec(),
            Some(parent) => {
                let iss = Issuer::from_params(&parent.params, &parent.key);
                params.signed_by(&key, &iss).unwrap().der().to_vec()
            }
        };
        GenCert { der, key, params }
    }

    /// root → intermediate(pathlen 0) → leaf, mirroring the AWS shape but generated.
    fn gen_chain() -> (Vec<u8>, Vec<Vec<u8>>, Vec<u8>) {
        let root = gen_cert("test root", Some(BasicConstraints::Unconstrained), None);
        let inter = gen_cert(
            "test intermediate",
            Some(BasicConstraints::Constrained(0)),
            Some(&root),
        );
        let leaf = gen_cert("test leaf", None, Some(&inter));
        (root.der.clone(), vec![root.der, inter.der], leaf.der)
    }

    /// Any time inside the generated certs' default validity window.
    fn gen_now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    #[test]
    fn verifies_generated_chain() {
        let (root, cabundle, leaf) = gen_chain();
        verify_cert_chain_against(&leaf, &cabundle, &root, gen_now()).unwrap();
    }

    /// `webpki` never checks a trust anchor's own validity window, so an expired pinned root
    /// has to be rejected by `check_root_validity` or it keeps validating chains forever.
    #[test]
    fn rejects_expired_root_anchor() {
        let mut root = gen_cert("expired root", Some(BasicConstraints::Unconstrained), None);
        root.params.not_before = rcgen::date_time_ymd(2000, 1, 1);
        root.params.not_after = rcgen::date_time_ymd(2001, 1, 1);
        root.der = root.params.self_signed(&root.key).unwrap().der().to_vec();

        let inter = gen_cert(
            "test intermediate",
            Some(BasicConstraints::Constrained(0)),
            Some(&root),
        );
        let leaf = gen_cert("test leaf", None, Some(&inter));
        let cabundle = vec![root.der.clone(), inter.der];

        let err =
            verify_cert_chain_against(&leaf.der, &cabundle, &root.der, gen_now()).unwrap_err();
        match err {
            AttestationError::CertExpired(msg) => {
                assert!(msg.contains("pinned root CA"), "got: {msg}")
            }
            other => panic!("expected CertExpired, got: {other:?}"),
        }
    }

    /// The pinned AWS root must be inside its window at the timestamp the AWS fixtures use.
    #[test]
    fn pinned_aws_root_is_currently_valid() {
        let root = aws_root_ca_der().unwrap();
        check_root_validity(&root, gen_now()).unwrap();
    }

    /// CVE-2021-3450 class: an end-entity cert must not be usable as an intermediate.
    #[test]
    fn rejects_leaf_used_as_intermediate() {
        let root = gen_cert("test root", Some(BasicConstraints::Unconstrained), None);
        let not_a_ca = gen_cert("impostor", None, Some(&root));
        let leaf = gen_cert("test leaf", None, Some(&not_a_ca));
        let cabundle = vec![root.der.clone(), not_a_ca.der];

        let err =
            verify_cert_chain_against(&leaf.der, &cabundle, &root.der, gen_now()).unwrap_err();
        match err {
            AttestationError::CertChain(msg) => {
                assert!(msg.contains("EndEntityUsedAsCa"), "got: {msg}")
            }
            other => panic!("expected CertChain, got: {other:?}"),
        }
    }

    #[test]
    fn rejects_intermediate_without_key_cert_sign() {
        let root = gen_cert("test root", Some(BasicConstraints::Unconstrained), None);
        let mut inter = gen_cert(
            "no keyCertSign",
            Some(BasicConstraints::Constrained(0)),
            Some(&root),
        );
        // Re-issue the same CA but with a keyUsage that forbids signing certs.
        inter.params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        let iss = Issuer::from_params(&root.params, &root.key);
        inter.der = inter
            .params
            .signed_by(&inter.key, &iss)
            .unwrap()
            .der()
            .to_vec();
        let leaf = gen_cert("test leaf", None, Some(&inter));

        let err = verify_cert_chain_against(
            &leaf.der,
            &[root.der.clone(), inter.der],
            &root.der,
            gen_now(),
        )
        .unwrap_err();
        match err {
            AttestationError::CertChain(msg) => {
                assert!(msg.contains("keyCertSign"), "got: {msg}")
            }
            other => panic!("expected CertChain, got: {other:?}"),
        }
    }

    /// The constraint sits on an intermediate because webpki does not carry the trust anchor's
    /// own basicConstraints into validation. The real AWS root asserts no `pathLenConstraint`.
    #[test]
    fn rejects_path_len_exceeded() {
        // pathlen:0 permits no further CAs, but a second intermediate follows.
        let root = gen_cert("test root", Some(BasicConstraints::Unconstrained), None);
        let a = gen_cert(
            "inter a",
            Some(BasicConstraints::Constrained(0)),
            Some(&root),
        );
        let b = gen_cert("inter b", Some(BasicConstraints::Unconstrained), Some(&a));
        let leaf = gen_cert("test leaf", None, Some(&b));

        let err = verify_cert_chain_against(
            &leaf.der,
            &[root.der.clone(), a.der, b.der],
            &root.der,
            gen_now(),
        )
        .unwrap_err();
        match err {
            AttestationError::CertChain(msg) => {
                assert!(msg.contains("PathLenConstraintViolated"), "got: {msg}")
            }
            other => panic!("expected CertChain, got: {other:?}"),
        }
    }

    /// A P-256 cert is signed with ecdsa-with-SHA256, which the pin must reject.
    #[test]
    fn rejects_non_p384_signature_algorithm() {
        let root_key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let mut root_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        root_params
            .distinguished_name
            .push(DnType::CommonName, "p256 root");
        root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        root_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let root_der = root_params.self_signed(&root_key).unwrap().der().to_vec();
        let root = GenCert {
            der: root_der.clone(),
            key: root_key,
            params: root_params,
        };

        let inter = gen_cert(
            "p256-signed intermediate",
            Some(BasicConstraints::Constrained(0)),
            Some(&root),
        );
        let leaf = gen_cert("test leaf", None, Some(&inter));

        let err = verify_cert_chain_against(
            &leaf.der,
            &[root.der.clone(), inter.der],
            &root.der,
            gen_now(),
        )
        .unwrap_err();
        match err {
            AttestationError::CertChain(msg) => {
                assert!(msg.contains("UnsupportedSignatureAlgorithm"), "got: {msg}")
            }
            other => panic!("expected CertChain, got: {other:?}"),
        }
    }

    /// Names must chain, not just signatures.
    ///
    /// Signed with the root's key but carrying an unrelated issuer DN, so the signature check
    /// passes and only the name comparison can reject it. Swapping in a foreign certificate
    /// instead would break the signature too, and would not test this at all.
    #[test]
    fn rejects_issuer_name_mismatch() {
        let root = gen_cert("test root", Some(BasicConstraints::Unconstrained), None);
        let decoy = gen_cert("decoy root", Some(BasicConstraints::Unconstrained), None);

        let key = KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params
            .distinguished_name
            .push(DnType::CommonName, "misnamed intermediate");
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        // Issuer name from the decoy, signing key from the real root.
        let iss = Issuer::from_params(&decoy.params, &root.key);
        let inter_der = params.signed_by(&key, &iss).unwrap().der().to_vec();
        let inter = GenCert {
            der: inter_der.clone(),
            key,
            params,
        };
        let leaf = gen_cert("test leaf", None, Some(&inter));

        let err = verify_cert_chain_against(
            &leaf.der,
            &[root.der.clone(), inter_der],
            &root.der,
            gen_now(),
        )
        .unwrap_err();
        match err {
            AttestationError::CertChain(msg) => {
                assert!(msg.contains("UnknownIssuer"), "got: {msg}")
            }
            other => panic!("expected CertChain, got: {other:?}"),
        }
    }

    /// AWS §3.2.3.3: the target certificate must assert `digitalSignature`.
    #[test]
    fn rejects_leaf_without_digital_signature() {
        let root = gen_cert("test root", Some(BasicConstraints::Unconstrained), None);
        let inter = gen_cert(
            "test intermediate",
            Some(BasicConstraints::Constrained(0)),
            Some(&root),
        );

        let key = KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params
            .distinguished_name
            .push(DnType::CommonName, "no digitalSignature");
        params.is_ca = IsCa::ExplicitNoCa;
        params.key_usages = vec![KeyUsagePurpose::KeyEncipherment];
        let iss = Issuer::from_params(&inter.params, &inter.key);
        let leaf = params.signed_by(&key, &iss).unwrap().der().to_vec();

        let err =
            verify_cert_chain_against(&leaf, &[root.der.clone(), inter.der], &root.der, gen_now())
                .unwrap_err();
        match err {
            AttestationError::CertChain(msg) => {
                assert!(msg.contains("digitalSignature"), "got: {msg}")
            }
            other => panic!("expected CertChain, got: {other:?}"),
        }
    }

    /// AWS §3.2.3.3: the target certificate must carry no `pathLenConstraint`.
    ///
    /// rcgen ties `pathLenConstraint` to `IsCa::Ca`, so any chain it builds here trips
    /// `CaUsedAsEndEntity` first. Asserting the CA rejection is what this can honestly cover.
    #[test]
    fn rejects_leaf_asserting_ca() {
        let root = gen_cert("test root", Some(BasicConstraints::Unconstrained), None);
        let inter = gen_cert(
            "test intermediate",
            Some(BasicConstraints::Constrained(0)),
            Some(&root),
        );
        let ca_leaf = gen_cert(
            "ca as leaf",
            Some(BasicConstraints::Constrained(0)),
            Some(&inter),
        );

        let err = verify_cert_chain_against(
            &ca_leaf.der,
            &[root.der.clone(), inter.der],
            &root.der,
            gen_now(),
        )
        .unwrap_err();
        match err {
            AttestationError::CertChain(msg) => {
                assert!(msg.contains("CaUsedAsEndEntity"), "got: {msg}")
            }
            other => panic!("expected CertChain, got: {other:?}"),
        }
    }

    /// AWS §3.2.3.3: every CA must assert `keyCertSign`, so a CA with no keyUsage at all is
    /// rejected rather than waved through.
    #[test]
    fn rejects_ca_without_key_usage() {
        let root = gen_cert("test root", Some(BasicConstraints::Unconstrained), None);
        let mut inter = gen_cert(
            "no keyUsage",
            Some(BasicConstraints::Constrained(0)),
            Some(&root),
        );
        inter.params.key_usages = vec![];
        let iss = Issuer::from_params(&root.params, &root.key);
        inter.der = inter
            .params
            .signed_by(&inter.key, &iss)
            .unwrap()
            .der()
            .to_vec();
        let leaf = gen_cert("test leaf", None, Some(&inter));

        let err = verify_cert_chain_against(
            &leaf.der,
            &[root.der.clone(), inter.der],
            &root.der,
            gen_now(),
        )
        .unwrap_err();
        match err {
            AttestationError::CertChain(msg) => {
                assert!(msg.contains("missing keyUsage"), "got: {msg}")
            }
            other => panic!("expected CertChain, got: {other:?}"),
        }
    }

    /// The generated equivalent of the ordering bug, independent of the AWS fixture.
    #[test]
    fn rejects_generated_chain_reversed() {
        let (root, cabundle, leaf) = gen_chain();
        let mut reversed = cabundle;
        reversed.reverse();
        let err = verify_cert_chain_against(&leaf, &reversed, &root, gen_now()).unwrap_err();
        assert!(
            matches!(err, AttestationError::CertChain(_)),
            "got: {err:?}"
        );
    }

    /// Exercises real ECDSA P-384 signature linkage over an actual AWS chain, which the
    /// synthetic tests above cannot do.
    #[test]
    fn verifies_real_aws_cert_chain() {
        verify_cert_chain_at(AWS_LEAF, &aws_cabundle(), AWS_CHAIN_VALID_AT).unwrap();
    }

    /// The bug this fixture is here to catch: reversed, the root no longer anchors.
    #[test]
    fn rejects_real_aws_chain_in_leaf_first_order() {
        let mut reversed = aws_cabundle();
        reversed.reverse();
        let err = verify_cert_chain_at(AWS_LEAF, &reversed, AWS_CHAIN_VALID_AT).unwrap_err();
        assert!(
            matches!(err, AttestationError::CertChain(_)),
            "got: {err:?}"
        );
    }

    /// webpki builds the path itself, so a swapped bundle still verifies. The pinned anchor is
    /// what makes the chain trustworthy, not the ordering.
    #[test]
    fn verifies_real_aws_chain_with_reordered_intermediates() {
        let mut swapped = aws_cabundle();
        swapped.swap(1, 2);
        verify_cert_chain_at(AWS_LEAF, &swapped, AWS_CHAIN_VALID_AT).unwrap();
    }

    #[test]
    fn rejects_real_aws_chain_outside_validity_window() {
        let err = verify_cert_chain_at(AWS_LEAF, &aws_cabundle(), AWS_CHAIN_VALID_AT + 86_400)
            .unwrap_err();
        assert!(
            matches!(err, AttestationError::CertExpired(_)),
            "got: {err:?}"
        );
    }
}
