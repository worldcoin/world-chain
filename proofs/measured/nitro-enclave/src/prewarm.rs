//! Pre-warming the on-chain [`CertManager`] certificate cache.
//!
//! `NitroValidator.validateAttestationWithHints` — the function behind
//! `NitroEnclaveKeyRegistry.registerKey` — re-walks the attestation's certificate bundle via
//! `verifyCachedCertBundle`, passing **empty** hint streams. That only succeeds on certificates
//! already present in the `CertManager` cache; an uncached certificate falls through to
//! signature verification against an empty hint stream and reverts with
//! `"inverse hint underflow"`, even when the attestation's own signature hints are valid.
//!
//! AWS rotates the enclave's leaf certificate roughly every three hours, so the cache goes cold
//! on its own. Rather than requiring an operator to run a pre-warm step inside that window, this
//! module lets the worker pre-warm its *own* chain: [`build_prewarm_plan`] turns an attestation
//! document into the ordered list of `verifyCACertWithHints` / `verifyClientCertWithHints` calls
//! needed to make `registerKey` succeed.
//!
//! The pinned AWS root CA is written into the cache by the `CertManager` constructor, so it is
//! never part of a plan — it only seeds the parent hash for the next certificate in the chain.

use alloy_primitives::{B256, b256, keccak256};
use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha384};

use crate::{attestation::cert_pubkey_xy, p384_hints::collect_hints};

/// `keccak256` of the pinned AWS Nitro root CA certificate.
///
/// Mirrors `CertManager.ROOT_CA_CERT_HASH`. The root is pre-cached in the `CertManager`
/// constructor and is keyed by this constant rather than by its TBS hash.
pub const ROOT_CA_CERT_HASH: B256 =
    b256!("311d96fcd5c5e0ccf72ef548e2ea7d4c0cd53ad7c4cc49e67471aed41d61f185");

/// A certificate that must be verified into the `CertManager` cache before `registerKey`
/// can succeed, together with everything needed to submit it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColdCert {
    /// DER-encoded certificate.
    pub cert: Vec<u8>,
    /// Cache key of the parent certificate (`ROOT_CA_CERT_HASH` for the root's children).
    pub parent_hash: B256,
    /// This certificate's own cache key, used to check whether it is already cached.
    pub cache_key: B256,
    /// Off-chain modular-inverse hints for this certificate's P-384 signature.
    pub hints: Vec<u8>,
    /// `true` for CA certificates (`verifyCACertWithHints`), `false` for the end-entity leaf
    /// (`verifyClientCertWithHints`).
    pub is_ca: bool,
}

/// Computes the `CertManager` cache key for a DER-encoded certificate.
///
/// Mirrors `CertManager._certCacheKey`: the pinned root is keyed by [`ROOT_CA_CERT_HASH`];
/// every other certificate is keyed by `keccak256` over its TBSCertificate element, header
/// included. Keying on the TBS rather than the whole certificate makes the key invariant to
/// ECDSA signature malleability.
///
/// # Errors
///
/// Returns an error if `der` is not a parseable X.509 certificate.
pub fn cert_cache_key(der: &[u8]) -> Result<B256> {
    use x509_parser::prelude::{FromDer as _, X509Certificate};

    if keccak256(der) == ROOT_CA_CERT_HASH {
        return Ok(ROOT_CA_CERT_HASH);
    }

    let (_, cert) =
        X509Certificate::from_der(der).map_err(|e| anyhow!("X.509 parse error: {e:?}"))?;
    Ok(keccak256(cert.tbs_certificate.as_ref()))
}

/// Parses a DER-encoded X.509 certificate into `(sha384(tbs), r || s)`.
///
/// The certificate signature covers the DER encoding of the TBSCertificate element (header
/// included); the signature itself is a DER `SEQUENCE { INTEGER r, INTEGER s }` which is
/// decoded here into the raw 96-byte `r || s` form the hint generator expects.
///
/// # Errors
///
/// Returns an error if the certificate or its signature cannot be parsed, or if either
/// signature component exceeds 384 bits.
pub fn parse_cert_signature(der: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    use x509_parser::prelude::{FromDer as _, X509Certificate};

    let (_, cert) =
        X509Certificate::from_der(der).map_err(|e| anyhow!("X.509 parse error: {e:?}"))?;

    let hash = Sha384::digest(cert.tbs_certificate.as_ref()).to_vec();
    let sig = decode_ecdsa_der_sig(cert.signature_value.data.as_ref())
        .context("decoding certificate ECDSA signature")?;

    Ok((hash, sig))
}

/// Builds the ordered set of certificates that must be cached before `registerKey` succeeds.
///
/// The returned entries are in dependency order: each one's `parent_hash` refers to a
/// certificate that either is already cached or appears earlier in the list. Callers should
/// skip entries whose `cache_key` is already present in the cache.
///
/// # Errors
///
/// Returns an error if the attestation document is malformed, or if its `cabundle` does not
/// begin with the pinned AWS root CA (AWS orders `cabundle` root-first, and the on-chain walk
/// depends on that ordering).
pub fn build_prewarm_plan(attestation_doc: &[u8]) -> Result<Vec<ColdCert>> {
    let parsed = crate::attestation::parse_attestation_doc(attestation_doc)
        .map_err(|e| anyhow!("parsing attestation document: {e}"))?;

    let leaf = parsed.certificate;
    if parsed.cabundle.is_empty() {
        bail!("attestation document has an empty `cabundle`; cannot build a pre-warm plan");
    }

    let mut plan = Vec::with_capacity(parsed.cabundle.len());
    let mut parent_hash = B256::ZERO;
    // Public key of the most recently walked certificate, used to generate the next one's
    // signature hints. `None` until the pinned root has been seen.
    let mut parent_pubkey: Option<[u8; 96]> = None;

    for (i, cert) in parsed.cabundle.iter().enumerate() {
        let cache_key = cert_cache_key(cert)
            .with_context(|| format!("computing cache key for cabundle[{i}]"))?;

        // The root is pinned by the CertManager constructor: it is always cached, needs no
        // hints, and costs no transaction. It only seeds the parent for the next certificate.
        if cache_key == ROOT_CA_CERT_HASH {
            parent_hash = cache_key;
            parent_pubkey = Some(
                cert_pubkey_xy(cert).map_err(|e| anyhow!("extracting root CA public key: {e}"))?,
            );
            continue;
        }

        let pubkey = parent_pubkey.ok_or_else(|| {
            anyhow!(
                "cabundle[{i}] has no verified parent — the pinned AWS root CA must be \
                 cabundle[0], but it was not found there"
            )
        })?;

        let (tbs_hash, sig) = parse_cert_signature(cert)
            .with_context(|| format!("parsing cabundle[{i}] signature"))?;
        let hints = collect_hints(&tbs_hash, &sig, &pubkey)
            .with_context(|| format!("generating P-384 hints for cabundle[{i}]"))?;

        plan.push(ColdCert {
            cert: cert.to_vec(),
            parent_hash,
            cache_key,
            hints,
            is_ca: true,
        });

        parent_hash = cache_key;
        parent_pubkey = Some(
            cert_pubkey_xy(cert)
                .map_err(|e| anyhow!("extracting cabundle[{i}] public key: {e}"))?,
        );
    }

    let pubkey = parent_pubkey.ok_or_else(|| {
        anyhow!("cabundle does not contain the pinned AWS root CA; cannot verify the leaf")
    })?;
    let (tbs_hash, sig) = parse_cert_signature(&leaf).context("parsing leaf signature")?;
    let hints = collect_hints(&tbs_hash, &sig, &pubkey)
        .context("generating P-384 hints for the leaf certificate")?;

    plan.push(ColdCert {
        cache_key: cert_cache_key(&leaf).context("computing cache key for the leaf cert")?,
        cert: leaf.into_vec(),
        parent_hash,
        hints,
        is_ca: false,
    });

    Ok(plan)
}

/// Decodes the `notAfter` field out of a packed `CertManager.VerifiedCert` record.
///
/// The contract stores records as `abi.encodePacked(ca, notAfter, maxPathLen, subjectHash,
/// pubKey)`, so `notAfter` is the big-endian `uint64` at bytes `1..9`. Returns `None` for an
/// empty (uncached) record or a record too short to carry the field.
pub fn packed_cert_not_after(packed: &[u8]) -> Option<u64> {
    let bytes: [u8; 8] = packed.get(1..9)?.try_into().ok()?;
    Some(u64::from_be_bytes(bytes))
}

// ─── DER helpers ─────────────────────────────────────────────────────────────

/// Decodes a DER `SEQUENCE { INTEGER r, INTEGER s }` into raw `r || s`, each left-padded to
/// 48 bytes.
fn decode_ecdsa_der_sig(der: &[u8]) -> Result<Vec<u8>> {
    if der.first() != Some(&0x30) {
        bail!(
            "expected SEQUENCE tag 0x30, got 0x{:02x}",
            der.first().copied().unwrap_or(0)
        );
    }
    let mut pos = 1;
    let (seq_len, consumed) = decode_der_length(&der[pos..])?;
    pos += consumed;
    let end = pos
        .checked_add(seq_len)
        .filter(|end| *end <= der.len())
        .ok_or_else(|| anyhow!("DER SEQUENCE length overflows input"))?;

    let (r_bytes, advanced) = decode_der_integer(&der[pos..end])?;
    pos += advanced;
    let (s_bytes, _) = decode_der_integer(&der[pos..end])?;

    let mut out = Vec::with_capacity(96);
    out.extend_from_slice(&pad_to_48(&r_bytes)?);
    out.extend_from_slice(&pad_to_48(&s_bytes)?);
    Ok(out)
}

fn decode_der_length(data: &[u8]) -> Result<(usize, usize)> {
    let first = *data
        .first()
        .ok_or_else(|| anyhow!("unexpected end of DER length"))?;
    if first < 0x80 {
        return Ok((first as usize, 1));
    }
    let num_bytes = (first & 0x7f) as usize;
    if num_bytes == 0 || num_bytes > 4 || data.len() < 1 + num_bytes {
        bail!("unsupported DER length encoding");
    }
    let mut len = 0usize;
    for &b in &data[1..=num_bytes] {
        len = (len << 8) | b as usize;
    }
    Ok((len, 1 + num_bytes))
}

fn decode_der_integer(data: &[u8]) -> Result<(Vec<u8>, usize)> {
    if data.first() != Some(&0x02) {
        bail!(
            "expected INTEGER tag 0x02, got 0x{:02x}",
            data.first().copied().unwrap_or(0)
        );
    }
    let (len, header) = decode_der_length(&data[1..])?;
    let start = 1 + header;
    let bytes = data
        .get(start..start + len)
        .ok_or_else(|| anyhow!("DER INTEGER length overflows input"))?;
    // DER prefixes a zero byte to keep the sign bit clear on positive integers.
    let stripped = match bytes.first() {
        Some(0x00) => &bytes[1..],
        _ => bytes,
    };
    Ok((stripped.to_vec(), start + len))
}

fn pad_to_48(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() > 48 {
        bail!("integer exceeds 384 bits ({} bytes)", bytes.len());
    }
    let mut out = vec![0u8; 48 - bytes.len()];
    out.extend_from_slice(bytes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pinned root's cache key is the constant, not its TBS hash — matching
    /// `CertManager._certCacheKey`'s short-circuit.
    #[test]
    fn root_ca_keys_to_the_pinned_constant() {
        let root = include_bytes!("testdata/aws_root.der");
        assert_eq!(keccak256(root), ROOT_CA_CERT_HASH);
        assert_eq!(cert_cache_key(root).unwrap(), ROOT_CA_CERT_HASH);
    }

    /// Non-root certificates key on the TBSCertificate element, so the key differs from a
    /// plain hash of the whole DER.
    #[test]
    fn non_root_keys_on_tbs_not_whole_cert() {
        let cert = include_bytes!("testdata/aws_zonal.der");
        let key = cert_cache_key(cert).unwrap();
        assert_ne!(key, keccak256(cert));
        assert_ne!(key, ROOT_CA_CERT_HASH);
    }

    #[test]
    fn cert_signature_decodes_to_96_bytes() {
        let cert = include_bytes!("testdata/aws_zonal.der");
        let (hash, sig) = parse_cert_signature(cert).unwrap();
        assert_eq!(hash.len(), 48, "SHA-384 digest");
        assert_eq!(sig.len(), 96, "r || s");
    }

    #[test]
    fn rejects_malformed_certificate() {
        assert!(cert_cache_key(&[0u8; 8]).is_err());
        assert!(parse_cert_signature(&[0u8; 8]).is_err());
    }

    #[test]
    fn packed_not_after_reads_the_uint64_at_offset_one() {
        // ca (1 byte) || notAfter (8 bytes) || rest
        let mut packed = vec![0x01];
        packed.extend_from_slice(&1_787_419_975u64.to_be_bytes());
        packed.extend_from_slice(&[0u8; 40]);
        assert_eq!(packed_cert_not_after(&packed), Some(1_787_419_975));

        assert_eq!(packed_cert_not_after(&[]), None, "uncached record");
        assert_eq!(packed_cert_not_after(&[0x01, 0x00]), None, "truncated");
    }

    #[test]
    fn der_integer_strips_the_sign_padding_byte() {
        // INTEGER 0x00FF → 0xFF once the sign byte is stripped.
        let (bytes, consumed) = decode_der_integer(&[0x02, 0x02, 0x00, 0xFF]).unwrap();
        assert_eq!(bytes, vec![0xFF]);
        assert_eq!(consumed, 4);
    }

    #[test]
    fn der_length_rejects_overflowing_sequence() {
        // SEQUENCE claiming 0x7F bytes of content but carrying none.
        assert!(decode_ecdsa_der_sig(&[0x30, 0x7F]).is_err());
    }
}
