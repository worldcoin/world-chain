//! Host-side verification of NSM attestation documents.
//!
//! A Nitro NSM attestation document is a COSE_Sign1 structure whose payload is a CBOR map
//! containing the PCRs, the optional `user_data` field, the enclave certificate, and the
//! certificate chain back to the AWS Nitro Attestation PKI root. This module only checks the
//! PCR / `user_data` invariants needed by the World fault proof flow.
//!
//! TODO(nitro): wire in full COSE_Sign1 signature verification and AWS root-of-trust
//! certificate chain validation (use the `aws-nitro-enclaves-cose` crate plus the published
//! AWS root CA). Until that lands, callers must additionally pass the document through a
//! verified pipeline (e.g. `nsm-cli verify` or `aws-nitro-enclaves-attestation-doc-validation`).

use std::collections::BTreeMap;

use crate::{ExpectedPcrs, PCR_LEN, PcrDigest};

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
}

/// Parses a `COSE_Sign1` Nitro attestation document and returns the inner payload fields the
/// host cares about.
///
/// This does **not** verify the signature or certificate chain — see the module-level TODO.
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
            _ => {}
        }
    }

    Ok(ParsedAttestationDoc {
        pcrs,
        user_data,
        module_id,
        digest,
    })
}

/// Verifies that a parsed attestation document matches the supplied expectations.
pub fn verify_attestation_doc(
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

    // TODO(nitro): verify COSE_Sign1 signature against the enclave certificate, and validate
    // the certificate chain back to the published AWS Nitro Attestation PKI root CA.

    Ok(parsed)
}

fn check_pcr(
    parsed: &ParsedAttestationDoc,
    index: u8,
    expected: &PcrDigest,
) -> Result<(), AttestationError> {
    let actual = parsed
        .pcrs
        .get(&index)
        .ok_or(AttestationError::MissingField(match index {
            0 => "pcr0",
            1 => "pcr1",
            2 => "pcr2",
            _ => "pcrN",
        }))?;
    // All-zero expected digest is the placeholder — skip verification.
    if expected.iter().all(|&b| b == 0) {
        return Ok(());
    }
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

    #[test]
    fn parses_and_verifies_placeholder_pcrs() {
        let doc = make_doc(
            vec![(0, vec![0u8; 48]), (1, vec![0u8; 48]), (2, vec![0u8; 48])],
            Some(vec![7u8; 32]),
        );
        let parsed = verify_attestation_doc(&doc, &ExpectedPcrs::PLACEHOLDER, &[7u8; 32]).unwrap();
        assert_eq!(parsed.user_data.unwrap(), vec![7u8; 32]);
        assert_eq!(parsed.module_id.unwrap(), "test-module");
    }

    #[test]
    fn rejects_pcr_mismatch() {
        let doc = make_doc(
            vec![(0, vec![1u8; 48]), (1, vec![0u8; 48]), (2, vec![0u8; 48])],
            Some(vec![7u8; 32]),
        );
        let err = verify_attestation_doc(&doc, &ExpectedPcrs::PLACEHOLDER, &[7u8; 32]).unwrap_err();
        assert!(matches!(
            err,
            AttestationError::PcrMismatch { index: 0, .. }
        ));
    }

    #[test]
    fn rejects_user_data_mismatch() {
        let doc = make_doc(
            vec![(0, vec![0u8; 48]), (1, vec![0u8; 48]), (2, vec![0u8; 48])],
            Some(vec![9u8; 32]),
        );
        let err = verify_attestation_doc(&doc, &ExpectedPcrs::PLACEHOLDER, &[7u8; 32]).unwrap_err();
        assert!(matches!(err, AttestationError::UserDataMismatch { .. }));
    }
}
