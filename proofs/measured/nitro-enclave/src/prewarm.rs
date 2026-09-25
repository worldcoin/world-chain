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
/// signature scalar is outside the valid P-384 range.
pub fn parse_cert_signature(der: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    use x509_parser::prelude::{FromDer as _, X509Certificate};

    let (_, cert) =
        X509Certificate::from_der(der).map_err(|e| anyhow!("X.509 parse error: {e:?}"))?;

    let hash = Sha384::digest(cert.tbs_certificate.as_ref()).to_vec();
    let sig = p384::ecdsa::Signature::from_der(cert.signature_value.data.as_ref())
        .context("decoding certificate ECDSA signature")?
        .to_bytes()
        .to_vec();

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
    fn extracted_certificate_signatures_verify_against_their_issuers() {
        use p384::ecdsa::{Signature, VerifyingKey, signature::hazmat::PrehashVerifier};

        let chain = [
            include_bytes!("testdata/aws_root.der").as_slice(),
            include_bytes!("testdata/aws_regional.der").as_slice(),
            include_bytes!("testdata/aws_zonal.der").as_slice(),
            include_bytes!("testdata/aws_instance.der").as_slice(),
            include_bytes!("testdata/aws_leaf.der").as_slice(),
        ];
        for (index, certificate) in chain.iter().enumerate() {
            let issuer = chain[index.saturating_sub(1)];
            let mut public_key = vec![4];
            public_key.extend_from_slice(&cert_pubkey_xy(issuer).unwrap());
            let key = VerifyingKey::from_sec1_bytes(&public_key).unwrap();
            let (hash, signature) = parse_cert_signature(certificate).unwrap();
            let signature = Signature::from_slice(&signature).unwrap();
            key.verify_prehash(&hash, &signature).unwrap();
        }
    }

    #[test]
    fn rejects_invalid_signature_scalars_in_certificate() {
        use x509_parser::prelude::{FromDer as _, X509Certificate};

        let mut certificate = include_bytes!("testdata/aws_zonal.der").to_vec();
        let (_, parsed) = X509Certificate::from_der(&certificate).unwrap();
        let signature = parsed.signature_value.data.as_ref();
        let offset = certificate.len() - signature.len();
        // The fixture uses short DER lengths. Zeroing r preserves certificate framing.
        assert_eq!(&signature[..3], &[0x30, (signature.len() - 2) as u8, 0x02]);
        let r_len = signature[3] as usize;
        certificate[offset + 4..offset + 4 + r_len].fill(0);
        assert!(parse_cert_signature(&certificate).is_err());
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
}
