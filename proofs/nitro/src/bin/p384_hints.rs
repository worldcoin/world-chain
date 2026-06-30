//! CLI tool to generate P-384 modular-inverse hints for hinted on-chain
//! ECDSA384 verification (base/nitro-validator PR #28).
//!
//! ## Usage
//!
//! ```text
//! # Hints for a raw hash + signature + pubkey:
//! p384-hints verify --hash <hex> --signature <hex> --pubkey <hex>
//!
//! # Hints for an X.509 certificate signature (DER):
//! p384-hints cert --cert <@path|hex> --parent-pubkey <hex>
//!
//! # Hints for the final COSE_Sign1 attestation signature:
//! p384-hints attestation --attestation <@path|hex> --leaf-pubkey <hex>
//! ```
//!
//! All `--*` hex arguments accept an optional `0x` prefix or `@path` to read
//! from a file. Output is a `0x`-prefixed hex string written to stdout — pass
//! it as the `attestationSigHints` / `signatureHints` argument to the Solidity
//! contracts.
//!
//! ## Example (full key-registration flow)
//!
//! ```sh
//! # 1. Split the raw attestation document (CBOR COSE_Sign1)
//! #    into TBS + signature using cast / forge:
//! ATTESTATION_HEX=$(xxd -p -c0 attestation.bin)
//! TBS_AND_SIG=$(cast call $NITRO_VALIDATOR "decodeAttestationTbs(bytes)" \
//!     0x$ATTESTATION_HEX)
//!
//! # 2. Extract the leaf cert public key from the cabundle (off-chain parse).
//! LEAF_PUBKEY_HEX=...
//!
//! # 3. Pre-warm CertManager for each intermediate cert:
//! CA_HINTS=$(p384-hints cert --cert @root.der     --parent-pubkey $ROOT_PUBKEY)
//! cast send $CERT_MANAGER "verifyCACertWithHints(bytes,bytes32,bytes)" \
//!     0x$(xxd -p -c0 root.der) 0x0000...0000 $CA_HINTS
//!
//! # 4. Generate hints for the attestation signature:
//! ATTEST_HINTS=$(p384-hints attestation \
//!     --attestation @attestation.bin --leaf-pubkey $LEAF_PUBKEY_HEX)
//!
//! # 5. Register the key:
//! cast send $REGISTRY "registerKey(bytes,bytes,bytes)" \
//!     $TBS $SIG $ATTEST_HINTS
//! ```

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use world_chain_proof_nitro::p384_hints::collect_hints;

// ─── X.509 / COSE_Sign1 parsing helpers ─────────────────────────────────────

/// Parse a DER-encoded X.509 certificate and return `(tbs_hash, signature)`.
///
/// The certificate signature covers the DER encoding of the TBSCertificate
/// field (SHA-384).  The signature is the raw r || s (48 + 48 bytes) decoded
/// from the DER BIT STRING SEQUENCE { INTEGER r, INTEGER s }.
fn parse_cert_signature(der: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    use sha2::{Digest, Sha384};
    use x509_parser::prelude::*;

    let (_, cert) =
        X509Certificate::from_der(der).map_err(|e| anyhow::anyhow!("X.509 parse error: {e:?}"))?;

    // TBS covers the raw DER bytes of the TBSCertificate element.
    // x509-parser exposes the raw slice via `cert.tbs_certificate.as_ref()`.
    let tbs_der = cert.tbs_certificate.as_ref();
    // SHA-384 of the TBS
    let hash = Sha384::digest(tbs_der).to_vec();

    // Signature: DER SEQUENCE { INTEGER r, INTEGER s } → r || s (48 bytes each)
    let sig_der = cert.signature_value.data;
    let sig = decode_ecdsa_der_sig(sig_der.as_ref())?;

    Ok((hash, sig))
}

/// Decode a DER-encoded ECDSA signature SEQUENCE { INTEGER r, INTEGER s }
/// into raw `r || s` (each zero-padded to 48 bytes).
fn decode_ecdsa_der_sig(der: &[u8]) -> Result<Vec<u8>> {
    // Manual minimal DER decoder: SEQUENCE { INTEGER, INTEGER }
    if der.is_empty() || der[0] != 0x30 {
        bail!(
            "expected SEQUENCE tag 0x30, got 0x{:02x}",
            der.get(0).copied().unwrap_or(0)
        );
    }
    let mut pos = 1;
    let (seq_len, consumed) = decode_der_length(&der[pos..])?;
    pos += consumed;
    let end = pos + seq_len;
    if end > der.len() {
        bail!("DER SEQUENCE length overflows input");
    }

    let (r_bytes, new_pos) = decode_der_integer(&der[pos..end])?;
    pos += new_pos;
    let (s_bytes, _) = decode_der_integer(&der[pos..end])?;

    let r = pad_to_48(&r_bytes)?;
    let s = pad_to_48(&s_bytes)?;

    let mut out = Vec::with_capacity(96);
    out.extend_from_slice(&r);
    out.extend_from_slice(&s);
    Ok(out)
}

fn decode_der_length(data: &[u8]) -> Result<(usize, usize)> {
    if data.is_empty() {
        bail!("unexpected end of DER length");
    }
    if data[0] < 0x80 {
        return Ok((data[0] as usize, 1));
    }
    let num_bytes = (data[0] & 0x7f) as usize;
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
    if data.is_empty() || data[0] != 0x02 {
        bail!(
            "expected INTEGER tag 0x02, got 0x{:02x}",
            data.get(0).copied().unwrap_or(0)
        );
    }
    let (len, header) = decode_der_length(&data[1..])?;
    let start = 1 + header;
    let bytes = &data[start..start + len];
    // Strip leading zero byte (DER uses it to keep the sign bit clear for positive integers)
    let stripped = if !bytes.is_empty() && bytes[0] == 0x00 {
        &bytes[1..]
    } else {
        bytes
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

/// Parse a COSE_Sign1 Nitro attestation document and extract `(hash, signature)`.
///
/// The attestation TBS is the CBOR-encoded `["Signature1", protected, b"", payload]`
/// structure.  The hash is SHA-384 of the TBS bytes; the signature is r || s.
fn parse_attestation_signature(raw: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    use ciborium::value::Value;
    use sha2::{Digest, Sha384};

    // The raw document is a COSE_Sign1 CBOR tag (18) containing an array.
    let value: Value =
        ciborium::de::from_reader(raw).map_err(|e| anyhow::anyhow!("CBOR decode error: {e:?}"))?;

    // Unwrap optional tag
    let array = match value {
        Value::Tag(_, inner) => match *inner {
            Value::Array(a) => a,
            _ => bail!("expected CBOR array inside tag"),
        },
        Value::Array(a) => a,
        _ => bail!("expected CBOR array or tag at root"),
    };

    if array.len() < 4 {
        bail!("COSE_Sign1 must have 4 elements, got {}", array.len());
    }

    let protected = match &array[0] {
        Value::Bytes(b) => b.clone(),
        _ => bail!("protected header must be bytes"),
    };
    let payload = match &array[2] {
        Value::Bytes(b) => b.clone(),
        _ => bail!("payload must be bytes"),
    };
    let sig_bytes = match &array[3] {
        Value::Bytes(b) => b.clone(),
        _ => bail!("signature must be bytes"),
    };

    if sig_bytes.len() != 96 {
        bail!(
            "attestation signature must be 96 bytes, got {}",
            sig_bytes.len()
        );
    }

    // Reconstruct the TBS structure: ["Signature1", protected, b"", payload]
    let tbs: Value = Value::Array(vec![
        Value::Text("Signature1".to_string()),
        Value::Bytes(protected),
        Value::Bytes(vec![]),
        Value::Bytes(payload),
    ]);
    let mut tbs_bytes = Vec::new();
    ciborium::ser::into_writer(&tbs, &mut tbs_bytes)
        .map_err(|e| anyhow::anyhow!("CBOR encode error: {e:?}"))?;

    let hash = Sha384::digest(&tbs_bytes).to_vec();
    Ok((hash, sig_bytes))
}

// ─── Input decoding ──────────────────────────────────────────────────────────

fn decode_input(s: &str) -> Result<Vec<u8>> {
    if let Some(path) = s.strip_prefix('@') {
        let raw = fs::read_to_string(Path::new(path)).with_context(|| format!("reading {path}"))?;
        return decode_input(raw.trim());
    }
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    // Try hex first, fall back to base64
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit()) && s.len() % 2 == 0 {
        return Ok(hex::decode(s).context("hex decode")?);
    }
    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD
        .decode(s)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(s))
        .map_err(|e| anyhow::anyhow!("base64 decode: {e}"))?)
}

// ─── CLI ─────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "p384-hints",
    about = "Generate P-384 modular-inverse hints for hinted on-chain ECDSA384 verification",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate hints for a raw (hash, signature, pubkey) triple.
    Verify {
        /// SHA-384 hash (up to 48 bytes, hex / 0x-prefixed / @file)
        #[arg(long)]
        hash: String,
        /// 96-byte r||s signature (hex / 0x-prefixed / @file)
        #[arg(long)]
        signature: String,
        /// 96-byte x||y uncompressed public key (hex / 0x-prefixed / @file)
        #[arg(long)]
        pubkey: String,
    },
    /// Generate hints for an X.509 certificate's signature (DER-encoded).
    Cert {
        /// DER-encoded certificate (hex / 0x-prefixed / @file)
        #[arg(long)]
        cert: String,
        /// Parent certificate's 96-byte x||y public key (hex / 0x-prefixed / @file)
        #[arg(long)]
        parent_pubkey: String,
    },
    /// Generate hints for the final COSE_Sign1 attestation signature.
    Attestation {
        /// Raw COSE_Sign1 attestation document (hex / 0x-prefixed / @file)
        #[arg(long)]
        attestation: String,
        /// Leaf certificate's 96-byte x||y public key (hex / 0x-prefixed / @file)
        #[arg(long)]
        leaf_pubkey: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let hints = match cli.command {
        Command::Verify {
            hash,
            signature,
            pubkey,
        } => {
            let hash = decode_input(&hash).context("--hash")?;
            let sig = decode_input(&signature).context("--signature")?;
            let pk = decode_input(&pubkey).context("--pubkey")?;
            collect_hints(&hash, &sig, &pk)?
        }
        Command::Cert {
            cert,
            parent_pubkey,
        } => {
            let cert_bytes = decode_input(&cert).context("--cert")?;
            let pk = decode_input(&parent_pubkey).context("--parent-pubkey")?;
            let (hash, sig) = parse_cert_signature(&cert_bytes).context("parsing cert")?;
            collect_hints(&hash, &sig, &pk)?
        }
        Command::Attestation {
            attestation,
            leaf_pubkey,
        } => {
            let attest = decode_input(&attestation).context("--attestation")?;
            let pk = decode_input(&leaf_pubkey).context("--leaf-pubkey")?;
            let (hash, sig) =
                parse_attestation_signature(&attest).context("parsing attestation")?;
            collect_hints(&hash, &sig, &pk)?
        }
    };

    println!("0x{}", hex::encode(&hints));
    Ok(())
}
