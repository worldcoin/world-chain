//! P-384 modular-inverse hint generator for on-chain hinted ECDSA384 verification.
//!
//! The [`base/nitro-validator`](https://github.com/base/nitro-validator) Solidity library
//! (PR #28) reduces the on-chain cost of P-384 signature verification from ~8 M gas to
//! ~1.5 M gas by accepting off-chain-computed modular-inverse *hints*. Each hint is a
//! 48-byte big-endian 384-bit integer `inv` such that `b · inv ≡ 1 (mod m)`. The contract
//! verifies every hint on-chain before use, so a malicious hint can only cause a revert —
//! never a false accept.
//!
//! This module reproduces the same hint-collection algorithm as
//! `lib/nitro-validator/tools/p384_hints.js`, in pure Rust.
//!
//! # Usage
//!
//! ```rust,ignore
//! use world_chain_proof_nitro::p384_hints::collect_hints;
//!
//! let hash      = hex::decode("...")?;   // SHA-384 digest, up to 48 bytes
//! let signature = hex::decode("...")?;   // 96 bytes: r || s (big-endian, each 48 bytes)
//! let pubkey    = hex::decode("...")?;   // 96 bytes: x || y (big-endian, each 48 bytes)
//!
//! let hints_bytes = collect_hints(&hash, &signature, &pubkey)?;
//! // Pass hints_bytes as `attestationSigHints` to registerKey / verifyAttestation.
//! ```

use anyhow::{Result, bail};
use num_bigint::BigUint;
use num_traits::{One, Zero};

// ─── P-384 curve parameters ──────────────────────────────────────────────────

/// Field prime p = 2^384 − 2^128 − 2^96 + 2^32 − 1
fn p() -> BigUint {
    BigUint::parse_bytes(
        b"FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFFFF0000000000000000FFFFFFFF",
        16,
    )
    .unwrap()
}

/// Group order n
fn n() -> BigUint {
    BigUint::parse_bytes(
        b"FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFC7634D81F4372DDF581A0DB248B0A77AECEC196ACCC52973",
        16,
    )
    .unwrap()
}

/// Curve coefficient a = p − 3
fn a_coeff() -> BigUint {
    BigUint::parse_bytes(
        b"FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFFFF0000000000000000FFFFFFFC",
        16,
    )
    .unwrap()
}

/// Curve coefficient b
fn b_coeff() -> BigUint {
    BigUint::parse_bytes(
        b"B3312FA7E23EE7E4988E056BE3F82D19181D9C6EFE8141120314088F5013875AC656398D8A2ED19D2A85C8EDD3EC2AEF",
        16,
    )
    .unwrap()
}

/// Base point G x-coordinate
fn gx() -> BigUint {
    BigUint::parse_bytes(
        b"AA87CA22BE8B05378EB1C71EF320AD746E1D3B628BA79B9859F741E082542A385502F25DBF55296C3A545E3872760AB7",
        16,
    )
    .unwrap()
}

/// Base point G y-coordinate
fn gy() -> BigUint {
    BigUint::parse_bytes(
        b"3617DE4A96262C6F5D9E98BF9292DC29F8F41DBD289A147CE9DA3113B5F0B8C00A60B1CE1D7E819D7A431D7C90EA0E5F",
        16,
    )
    .unwrap()
}

// ─── Big-integer helpers ─────────────────────────────────────────────────────

fn mod_add(a: &BigUint, b: &BigUint, m: &BigUint) -> BigUint {
    (a + b) % m
}

fn mod_sub(a: &BigUint, b: &BigUint, m: &BigUint) -> BigUint {
    if a >= b { a - b } else { m - b + a }
}

fn mod_mul(a: &BigUint, b: &BigUint, m: &BigUint) -> BigUint {
    (a * b) % m
}

fn mod_pow(base: &BigUint, exp: &BigUint, m: &BigUint) -> BigUint {
    base.modpow(exp, m)
}

/// Extended Euclidean modular inverse.  Returns `None` when `value` is not
/// invertible mod `modulus` (i.e., gcd > 1 or value == 0).
fn mod_inv(value: &BigUint, modulus: &BigUint) -> Option<BigUint> {
    if value.is_zero() {
        return None;
    }
    // Extended Euclidean algorithm (signed arithmetic via BigInt)
    use num_bigint::{BigInt, Sign};
    let v = BigInt::from_biguint(Sign::Plus, value.clone());
    let m = BigInt::from_biguint(Sign::Plus, modulus.clone());
    let mut low = v.clone() % &m;
    let mut high = m.clone();
    let mut lm = BigInt::one();
    let mut hm = BigInt::zero();

    while low > BigInt::one() {
        let ratio = &high / &low;
        let nm = &hm - &lm * &ratio;
        let nw = &high - &low * &ratio;
        hm = lm;
        high = low;
        lm = nm;
        low = nw;
    }

    if low != BigInt::one() {
        return None; // not invertible
    }

    let result = lm % &m;
    let result = if result < BigInt::zero() {
        result + m
    } else {
        result
    };
    result.to_biguint()
}

// ─── Hint collector ──────────────────────────────────────────────────────────

/// Collects modular-inverse hints and returns them.  Also computes the result
/// of the division `(a / b) mod m`.
fn record_inverse(hints: &mut Vec<BigUint>, b: &BigUint, m: &BigUint) -> Result<BigUint> {
    let b_norm = b % m;
    if b_norm.is_zero() {
        bail!("cannot invert zero");
    }
    let inv = mod_inv(&b_norm, m).ok_or_else(|| anyhow::anyhow!("value not invertible mod m"))?;
    hints.push(inv.clone());
    Ok(inv)
}

fn mod_div(hints: &mut Vec<BigUint>, a: &BigUint, b: &BigUint, m: &BigUint) -> Result<BigUint> {
    let inv = record_inverse(hints, b, m)?;
    Ok(mod_mul(a, &inv, m))
}

// ─── Affine point arithmetic ─────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct Point {
    x: BigUint,
    y: BigUint,
    infinity: bool,
}

impl Point {
    fn infinity() -> Self {
        Self {
            x: BigUint::zero(),
            y: BigUint::zero(),
            infinity: true,
        }
    }
    fn new(x: BigUint, y: BigUint) -> Self {
        Self {
            x,
            y,
            infinity: false,
        }
    }
}

fn twice_affine(
    hints: &mut Vec<BigUint>,
    p_field: &BigUint,
    a: &BigUint,
    pt: &Point,
) -> Result<Point> {
    if pt.infinity || pt.y.is_zero() {
        return Ok(Point::infinity());
    }
    // m = (3*x^2 + a) / (2*y)
    let x2 = mod_mul(&pt.x, &pt.x, p_field);
    let num = mod_add(&mod_mul(&BigUint::from(3u32), &x2, p_field), a, p_field);
    let den = mod_mul(&BigUint::from(2u32), &pt.y, p_field);
    let slope = mod_div(hints, &num, &den, p_field)?;

    let x3 = mod_sub(
        &mod_sub(&mod_mul(&slope, &slope, p_field), &pt.x, p_field),
        &pt.x,
        p_field,
    );
    let y3 = mod_sub(
        &mod_mul(&mod_sub(&pt.x, &x3, p_field), &slope, p_field),
        &pt.y,
        p_field,
    );
    Ok(Point::new(x3, y3))
}

fn add_affine(
    hints: &mut Vec<BigUint>,
    p_field: &BigUint,
    a: &BigUint,
    p1: &Point,
    p2: &Point,
) -> Result<Point> {
    if p1.infinity {
        return Ok(p2.clone());
    }
    if p2.infinity {
        return Ok(p1.clone());
    }

    if p1.x == p2.x {
        return if p1.y == p2.y {
            twice_affine(hints, p_field, a, p1)
        } else {
            Ok(Point::infinity())
        };
    }

    // slope = (y1 - y2) / (x1 - x2)
    let dy = mod_sub(&p1.y, &p2.y, p_field);
    let dx = mod_sub(&p1.x, &p2.x, p_field);
    let slope = mod_div(hints, &dy, &dx, p_field)?;

    let x3 = mod_sub(
        &mod_sub(&mod_mul(&slope, &slope, p_field), &p1.x, p_field),
        &p2.x,
        p_field,
    );
    let y3 = mod_sub(
        &mod_mul(&mod_sub(&p1.x, &x3, p_field), &slope, p_field),
        &p1.y,
        p_field,
    );
    Ok(Point::new(x3, y3))
}

// ─── Strauss-Shamir precomputed table ────────────────────────────────────────

/// Build the 8×8 precomputed point table as the JS tool does.
/// `points[i<<3 | j]` = `i·G + j·H` for i,j ∈ {0..7}.
fn precompute_table(
    hints: &mut Vec<BigUint>,
    p_field: &BigUint,
    a: &BigUint,
    hx: &BigUint,
    hy: &BigUint,
) -> Result<Vec<Point>> {
    let g = Point::new(gx(), gy());
    let h = Point::new(hx.clone(), hy.clone());

    let mut points = vec![Point::infinity(); 64];
    points[0x01] = h.clone(); // 0·G + 1·H
    points[0x08] = g.clone(); // 1·G + 0·H

    for i in 0usize..8 {
        for j in 0usize..8 {
            if i + j < 2 {
                continue;
            }
            let idx = (i << 3) | j;
            if i != 0 {
                let from = ((i - 1) << 3) | j;
                let prev = points[from].clone();
                points[idx] = add_affine(hints, p_field, a, &prev, &g)?;
            } else {
                let from = (i << 3) | (j - 1);
                let prev = points[from].clone();
                points[idx] = add_affine(hints, p_field, a, &prev, &h)?;
            }
        }
    }
    Ok(points)
}

// ─── Double scalar multiplication (Strauss-Shamir, 6-bit window) ─────────────

/// Triple-double: applies [`twice_affine`] three times with early exit when an
/// intermediate y-coordinate is zero, matching `_twice3Affine` in ECDSA384.sol.
fn twice3_affine(
    hints: &mut Vec<BigUint>,
    p_field: &BigUint,
    a: &BigUint,
    pt: &Point,
) -> Result<Point> {
    let p2 = twice_affine(hints, p_field, a, pt)?;
    if p2.infinity || p2.y.is_zero() {
        return Ok(Point::infinity());
    }
    let p3 = twice_affine(hints, p_field, a, &p2)?;
    if p3.infinity || p3.y.is_zero() {
        return Ok(Point::infinity());
    }
    twice_affine(hints, p_field, a, &p3)
}

/// Extracts a 3-bit window from `n` at bit position `shift` (i.e. `(n >> shift) & 7`).
fn bits3(n: &BigUint, shift: usize) -> usize {
    (n >> shift).to_bytes_le().first().copied().unwrap_or(0) as usize & 7
}

/// Strauss-Shamir double-scalar multiplication that mirrors the Solidity
/// `_doubleScalarMultiplication` and the JS `doubleScalarMultiplication` exactly.
///
/// The scalar is split into two machine words matching the U384 in-memory layout:
/// - **high word** (`scalar >> 256`): bits [383:256], processed with a 3-bit window
///   over the 184-bit range that the Solidity `mload(scalar)` covers.
/// - **low word** (`scalar & MASK_256`): bits [255:0], processed over the 256-bit range
///   that `mload(add(scalar, 0x20))` covers.
///
/// Each inner loop step triples the EC point (`twice3_affine`, 3 hints) before
/// adding the precomputed table entry for the current 6-bit mask (1 hint).  Two
/// single-doubling steps (`twice_affine`) bridge the boundary between words.
fn double_scalar_mul(
    hints: &mut Vec<BigUint>,
    p_field: &BigUint,
    a: &BigUint,
    points: &[Point],
    _scalar1: BigUint,
    _scalar2: BigUint,
) -> Result<Point> {
    let scalar1 = _scalar1;
    let scalar2 = _scalar2;

    let mask256 = (BigUint::one() << 256usize) - BigUint::one();
    // High 128 bits: bits [383:256] of each scalar (Solidity `mload(scalar)`).
    let s1h = &scalar1 >> 256usize;
    let s2h = &scalar2 >> 256usize;
    // Low 256 bits: bits [255:0] of each scalar (Solidity `mload(add(scalar, 0x20))`).
    let s1l = &scalar1 & &mask256;
    let s2l = &scalar2 & &mask256;

    let mut result = Point::infinity();

    // ── Phase 1: high 128 bits ───────────────────────────────────────────────
    result = twice_affine(hints, p_field, a, &result)?;

    // Bit 183 of a 128-bit value is always zero; included for algorithmic symmetry.
    let mask = (bits3(&s1h, 183) << 3) | bits3(&s2h, 183);
    if mask != 0 {
        result = add_affine(hints, p_field, a, &result, &points[mask])?;
    }

    for word in (4usize..=184).step_by(3) {
        result = twice3_affine(hints, p_field, a, &result)?;
        let shift = 184 - word;
        let mask = (bits3(&s1h, shift) << 3) | bits3(&s2h, shift);
        if mask != 0 {
            result = add_affine(hints, p_field, a, &result, &points[mask])?;
        }
    }

    // ── Phase 2: low 256 bits ────────────────────────────────────────────────
    result = twice_affine(hints, p_field, a, &result)?;

    let mask = (bits3(&s1l, 255) << 3) | bits3(&s2l, 255);
    if mask != 0 {
        result = add_affine(hints, p_field, a, &result, &points[mask])?;
    }

    for word in (4usize..=256).step_by(3) {
        result = twice3_affine(hints, p_field, a, &result)?;
        let shift = 256 - word;
        let mask = (bits3(&s1l, shift) << 3) | bits3(&s2l, shift);
        if mask != 0 {
            result = add_affine(hints, p_field, a, &result, &points[mask])?;
        }
    }

    Ok(result)
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Collect P-384 modular-inverse hints for a single ECDSA384 `verifyWithHints` call.
///
/// # Arguments
/// * `hash`      — SHA-384 digest of the message, up to 48 bytes (zero-padded on the left).
/// * `signature` — 96 bytes: `r || s`, each 48-byte big-endian.
/// * `pubkey`    — 96 bytes: `x || y`, each 48-byte big-endian uncompressed point coordinates.
///
/// # Returns
/// A byte vector that is the concatenation of 48-byte big-endian inverse hints.
/// Pass this as `attestationSigHints` / `signatureHints` to the Solidity contracts.
pub fn collect_hints(hash: &[u8], signature: &[u8], pubkey: &[u8]) -> Result<Vec<u8>> {
    if signature.len() != 96 {
        bail!("signature must be 96 bytes, got {}", signature.len());
    }
    if pubkey.len() != 96 {
        bail!("pubkey must be 96 bytes, got {}", pubkey.len());
    }
    if hash.len() > 48 {
        bail!("hash must be at most 48 bytes, got {}", hash.len());
    }

    let p_field = p();
    let n_order = n();
    let a = a_coeff();

    let r = BigUint::from_bytes_be(&signature[..48]);
    let s = BigUint::from_bytes_be(&signature[48..]);
    let pub_x = BigUint::from_bytes_be(&pubkey[..48]);
    let pub_y = BigUint::from_bytes_be(&pubkey[48..]);

    // Validate scalar bounds: r,s ∈ [1, n-1]; low-S not enforced (mirrors JS).
    if r.is_zero() || r >= n_order {
        bail!("r out of range");
    }
    if s.is_zero() || s >= n_order {
        bail!("s out of range");
    }

    // Validate public key is on curve: y^2 = x^3 + a·x + b (mod p)
    {
        if pub_x.is_zero() || pub_x >= p_field || pub_y.is_zero() || pub_y >= p_field {
            bail!("pubkey coordinates out of field range");
        }
        let lhs = mod_pow(&pub_y, &BigUint::from(2u32), &p_field);
        let rhs = mod_add(
            &mod_add(
                &mod_pow(&pub_x, &BigUint::from(3u32), &p_field),
                &mod_mul(&a, &pub_x, &p_field),
                &p_field,
            ),
            &b_coeff(),
            &p_field,
        );
        if lhs != rhs {
            bail!("pubkey is not on P-384");
        }
    }

    // Zero-pad hash to 48 bytes on the left
    let mut padded = vec![0u8; 48];
    padded[48 - hash.len()..].copy_from_slice(hash);
    let h = BigUint::from_bytes_be(&padded);

    let mut hints: Vec<BigUint> = Vec::new();

    // 1. scalar1 = h / s mod n
    let scalar1 = mod_div(&mut hints, &h, &s, &n_order)?;
    // 2. scalar2 = r / s mod n
    let scalar2 = mod_div(&mut hints, &r, &s, &n_order)?;

    // 3. Precompute point table (generates hints for each affine add/double)
    let table = precompute_table(&mut hints, &p_field, &a, &pub_x, &pub_y)?;

    // 4. Double-scalar multiplication
    let result = double_scalar_mul(&mut hints, &p_field, &a, &table, scalar1, scalar2)?;

    if result.infinity {
        bail!("scalar multiplication result is the point at infinity");
    }

    // Verify the signature
    let check = result.x % &n_order;
    if check != r {
        bail!("P-384 signature verification failed (hints generated but sig invalid)");
    }

    // Encode hints: each is 48 bytes big-endian
    let mut out = Vec::with_capacity(hints.len() * 48);
    for h_val in &hints {
        let bytes = h_val.to_bytes_be();
        if bytes.len() > 48 {
            bail!("hint value exceeds 384 bits");
        }
        // Left-pad to 48 bytes
        out.extend(std::iter::repeat_n(0u8, 48 - bytes.len()));
        out.extend_from_slice(&bytes);
    }

    Ok(out)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use p384::ecdsa::{Signature, SigningKey, signature::Signer};
    use sha2::{Digest, Sha384};

    /// Sign `message` with `signing_key` and return `(sha384_hash, r‖s, x‖y)`.
    fn sign_p384(signing_key: &SigningKey, message: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let sig: Signature = signing_key.sign(message);
        let hash = Sha384::digest(message).to_vec();

        // r‖s as raw 48+48 bytes
        let sig_bytes = sig.to_bytes();
        let signature = sig_bytes.to_vec();

        // Uncompressed public key: 0x04 ‖ x ‖ y  (97 bytes) → drop the 0x04 prefix → 96 bytes
        let verifying_key = signing_key.verifying_key();
        let encoded = verifying_key.to_encoded_point(false);
        let pubkey = encoded.as_bytes()[1..].to_vec(); // drop 0x04

        (hash, signature, pubkey)
    }

    /// `collect_hints` must produce output whose length is a multiple of 48
    /// (one 384-bit inverse per slot).
    #[test]
    fn hints_length_is_multiple_of_48() {
        let sk = SigningKey::random(&mut rand::thread_rng());
        let (hash, sig, pubkey) = sign_p384(&sk, b"hello world");
        let hints = collect_hints(&hash, &sig, &pubkey).expect("collect_hints failed");
        assert!(!hints.is_empty(), "hints must not be empty");
        assert_eq!(hints.len() % 48, 0, "each hint is 48 bytes");
    }

    /// Every 48-byte slot in the hint stream must be a valid modular inverse:
    /// `b · inv ≡ 1 (mod m)` where `m` alternates between the field prime `p`
    /// and the group order `n` depending on which operation generated it.
    /// We verify the weaker (but still meaningful) property that no hint slot
    /// is all-zeros (which would trivially fail the on-chain `b·inv ≡ 1` check).
    #[test]
    fn hints_are_nonzero() {
        let sk = SigningKey::random(&mut rand::thread_rng());
        let (hash, sig, pubkey) = sign_p384(&sk, b"test message");
        let hints = collect_hints(&hash, &sig, &pubkey).expect("collect_hints failed");
        for (i, chunk) in hints.chunks(48).enumerate() {
            assert_ne!(chunk, &[0u8; 48], "hint slot {i} must not be zero");
        }
    }

    /// The full round-trip: generate a P-384 keypair, sign a message, collect
    /// hints, then verify that each hint satisfies `b · inv ≡ 1 (mod m)` for
    /// the two moduli used during ECDSA384 verification (field prime `p` and
    /// group order `n`).  This mirrors the on-chain check in the Solidity
    /// `ECDSA384.moddivAssign` / `modinv` functions.
    ///
    /// We verify hints by re-running the ECDSA scalar arithmetic and checking
    /// that the signature verifies — if any hint were wrong the underlying
    /// modular arithmetic would produce a wrong result, so verification would
    /// fail.
    #[test]
    fn collect_hints_round_trip_verifies() {
        use p384::ecdsa::{VerifyingKey, signature::Verifier};

        let sk = SigningKey::random(&mut rand::thread_rng());
        let vk: VerifyingKey = *sk.verifying_key();
        let msg = b"nitro enclave attestation round-trip test";
        let (hash, sig_bytes, pubkey) = sign_p384(&sk, msg);

        // collect_hints internally re-runs ECDSA verification; it only succeeds
        // if the signature is valid AND all computed inverses are correct.
        let hints = collect_hints(&hash, &sig_bytes, &pubkey)
            .expect("collect_hints must succeed for a valid signature");

        // Sanity: hints are non-empty and well-formed.
        assert!(!hints.is_empty());
        assert_eq!(hints.len() % 48, 0);

        // Also confirm the signature itself verifies with the p384 crate.
        let sig = Signature::from_bytes(sig_bytes.as_slice().into()).unwrap();
        vk.verify(msg, &sig)
            .expect("p384 crate must also verify the same sig");
    }

    /// `collect_hints` must reject an all-zero signature.
    #[test]
    fn collect_hints_rejects_zero_r() {
        let sk = SigningKey::random(&mut rand::thread_rng());
        let (hash, _, pubkey) = sign_p384(&sk, b"test");
        let bad_sig = vec![0u8; 96];
        assert!(collect_hints(&hash, &bad_sig, &pubkey).is_err());
    }

    /// `collect_hints` must reject a pubkey that is not on the P-384 curve.
    #[test]
    fn collect_hints_rejects_off_curve_pubkey() {
        let sk = SigningKey::random(&mut rand::thread_rng());
        let (hash, sig, _) = sign_p384(&sk, b"test");
        let bad_pubkey = vec![1u8; 96]; // almost certainly not on the curve
        assert!(collect_hints(&hash, &sig, &bad_pubkey).is_err());
    }

    /// Deterministic test vector: private key = [0x01; 48] (all ones),
    /// message = b"p384_hints deterministic test vector v1".
    ///
    /// The expected hint bytes were generated by the `gen-test-vector` binary
    /// (src/bin/gen_test_vector.rs) and hardcoded here. Because P-384 signing
    /// uses RFC 6979 deterministic nonce generation, the same key + message
    /// always produces the same signature — so these expected hints are stable
    /// across platforms and Rust versions.
    ///
    /// If this test fails it means the hint-collection algorithm has changed;
    /// update the expected bytes by running:
    ///   cargo run -p world-chain-proof-nitro --bin gen-test-vector 2>/dev/null
    #[test]
    fn collect_hints_exact_output() {
        // SHA-384(b"p384_hints deterministic test vector v1")
        let hash = hex::decode("214d9fbfd0335b66c0ba2786c2c2acacfbed561810cde8a4a8156c9705958854e8dedb79e65a5ace24cf8439db9c87a4").unwrap();
        // r || s (48 bytes each), produced by RFC 6979 signing with sk=[0x01;48]
        let sig  = hex::decode("5faf432fc8d45308ae4b280f962516edf2556c6c5ddd08234d139b62de29494f557c37f332aa0e7b33184e6c8e598ce65f8825d7a2c99218726fb3ebc5acdfe4bc245e55879aa676f73280d486291e292aec586743541e7b0ab38a634b3c0b31").unwrap();
        // uncompressed public key x || y (without 0x04 prefix)
        let pubkey = hex::decode("43e3af2a0db9086750976877650f426d2157a45e10de646ff857198b226df0d4b2243408e03ba711d9c34c51cb344413dd12e3cea20d5112f06b0831d2ea139ba34061f8310e9744fd18d915ef34f6f2c670e34c63eeb80bcc613ecb91f2c196").unwrap();

        let hints = collect_hints(&hash, &sig, &pubkey).expect("collect_hints must succeed");

        assert_eq!(
            hints.len() % 48,
            0,
            "hint stream must be a multiple of 48 bytes"
        );
        assert_eq!(hints.len(), 27360, "expected 570 inverse hints × 48 bytes");

        let expected = hex::decode("cad7705dbf824e00676213cfa144e8d0eabb5c41f915f677fd17ef4a69025d814999cf2b6416266d5b44d828678bf4b0cad7705dbf824e00676213cfa144e8d0eabb5c41f915f677fd17ef4a69025d814999cf2b6416266d5b44d828678bf4b055c284da8c783664e32313ca00c5beeea8500da8760bdd256905f9969e95e198d91d6b26284f8d41419c268a22a33295df5fe23d6e21c9050b298ec44c676918500489d669ad3441777527283aa22c2c89286b7222b1a03179d59ca118b04ccfec49329b433b21a20abb0b1b2f4801f446e611459a3a0b83291cae73e030b9ec6b4923e7f2adf8d8051310d3cdf318c06f07e58b9394f2148de766a79a97e8201133cea3457b33d37004724af91503c6cafcb037f97eb449eb569b87d63061455d4745ac1deec45de2f3ca54d7ddd0ae783f6cec5747bb08bc3ddf3e14833de1e95477a397769821a18bd3f57144b5a6ccb8088328f199d54cf2650e9d5dee93e0453607e64cd369641ba1caed420a1026fb99e1c450a08004003b966892e53ad76e62a87f4ad4e7a1349b38cdb00573ee7716a426bdfa7b9344083ca59ee6821d0b84974281e1d4934795b00b7e6e3a44d79ace265825c93c11967f86632f45ecce13cc9f3c05053776e0c61067a204ddad0fde441eea033cd31af5f626452f3cef42df7867fe6bd52d2db3264639e10d74808b43bd4f546bd419a5f56bf21fe4b459db00397d2c3c80b85179c2b1be44f81d4ec1fa04052e703613b94940ae883d60ff4130b812bbc1da290060c3c49d70469ad0cd82bb6e788d31b7520dcd9059e85632a9dd1244cfd053a56990e8ec4b5b9a136d8974a9b446a395c8b1b2633045bf8ec7780a6170cf6681d441895f8557a61cc848233eba82ce958b537e3fd4ae3b4869172a8a1230fcf0dd3e181d8d2a219438eb4169027be0bbc225ff8ac997367d780d4fb0bc311ce18d80bb54e49bf2eec49c410a6ec1a8a86815a3f49bfdd29cefe3dfcd63f133a57dc7b2cf774118d2c89e45562d4781dc97f749fb682d2bbbe585fc3831c84f3f2b417b51339b593018f2003767804fafce41182f6f2a915529127d54c2598a7f9cbfaa5c55a323e834a4bd95d375bb9d13743ef8ed204531f2e40b6531b9c05c7670767bf04383a78b45e9ddb0f3250b94bbc29865fe2f35d65d317f8c72dbe8151f1ec2ab9626b69263c78270093546547e2d03d7a5005d9b577bd07c443fc64b64f24542843ecc552d9a4d30060dd6e47ef059aeb008bd38f3fa776da4947d134a73b908085bb8a42f2ab32ee5ad465859c33ab52afee7391ea56f1636b4c09fe28da2ce003afec6e4cd9c1e81221b7630c306cb00d975eccd7cb24650cb0ad740347153e4bf6528f6e2b2a2c66aadca83909b93403ddfd3e66eb200a0043491420f1db0baa37da6f855cd3a46886674a7c22c54052f8fd50d1351be39178eee40444c3a4f6d9f11c39b1594d90914586c513582df799c115a2da038cba9ab8275a21487bd9bd894f4b23471a6e07876550bf7ab981df0b9c1de73bd063e36c79b8c3c6631249bda90abdce36b3bafcc964f1f9a78ada0d9302475c3dba41051b27001f14aaea9f5f79e8b14a27901e0031ac18fd111ce7ff174570bfc53a4e972dd611e262ca53ba323709938b534cb994bfbab249782669182cdd312f06684bfc1f3bf867f1c392ace2ed7cfc970d444b1ef0dd92c08114793b1e675601c8fed81904495ef47a6429767b03fda49b3c203c9359c54111ed7811c8c6335fcc9ce1bbcf9175bb394b853a9cad0d674bf71a36b156c9c9e08152ac9df111ae7d2138930efa3ffeb4bff79113023f2907338589e83638a352c7d7606cbaa0dd40785d2bc0ab3efcb2b8b4a0c9c820d2260bb5013d833572b19991b77b2039a91688e8cee11291caa2841df61da6c8b92c4725abbb96315d67ba78e58139654d5819b0406b325ed8e9738971992d80b3f3922118eb6027a15349849bf08f704883e2f373db8b3fbda47cb850b3a443e9d650e92fb0073a0acbb3ad53d8e44ac5a3d4a18ea6b007966ea5356f763d1c036dc2ea56de45f8d949aef338c982328cb77a388f5048a88eb61414977d712cb10eca864a93a85df370926c267920b36792881f1836bbf8532fdbc2b5930ff59198fb3e8bd93b280c42c13cc44ebcd5c7e0f5ee8818b399cd005cc56f42143956b0ce8dd8daf23e1f3199d3625215a05316acaadf81435aca5217d3323cb0c239610d3860c9b19bca12734f7aee82559775f40e3bb8bfedd5799c2eeb78e89ed65621a647d6aa77453a347b3be3fc35808d174583d49f87f8a95ebc58f3996052cd86c7b0ebc7ff8ed51613ffe9d608bd0a11aec8bc6bfc05904c751d2278602a9bddfb705c3c2951106f8c1d861137a6e9f12db3a946559307eb6a7b2be338c44e2f794a6e59ac9c4316b956804073a9a5745a0c5f2e2f72a55f2abb9ec9e81646be93be34f204dc442cc044817c469d6cff3ad355114d1d995d45a973697ecda71733a3505180fc46af4c624185661a7dc273e4a0daf56d527c4c0e263ac8b3ae1b6cc75986eca6894f3b02233f1f39d41300ca21c77814a2d80b083cb0d7b8cb98d3384876870f060908ed6f4204928b9d7d90f1569ce91d5b5cc100bf1edfb1838c0ebb02806afdb8a16f8c9ba58bc1cdbb829117497e88f48e4b9847bfcbb0f2100a8fad73da322bf82ecd682c52492b32e6717ace15ef6c313eb0836c2583edf35f97779ac21e4ade56ec39ab77c7b83cc8d58718c15e21916b877d17d15ad0f8250f566578a6b27c4ef45fed21129959f44cbda09e2c41c04d8dc0af141dd50c22292ce5819b878fe74e1d981de48bdbed3f67dfdc31372c0f8efee5b3b14eb35e0ddfa2cc858e564e7e3cd87bceb0cdaebdba1c8a6de7cc5fc2da890b846621abe070e902d71087398e9a9875b088ec2e67494b98c52b6d1846a512cf165be8d33e17561d61cf90904eee0c242b186a1ed49cb515eb35536c0e59649ed16efe6cc1207a31da2ba74f29e3a424c2210e7f20eecad4657270381a6441994e0562c45d677da821880b28ec453294672571377caa5990b3165c0653e5b16c7c1683c6ea2f3d440f9bf72b2ffdfbbd03d65857fef26f7fab39c02180cbad7ec2b8df6ab113361cfd3ce7cfccf3a95c1a4e318d7dfee44f0e3a4174da5ad58abe883b5a3f8d06b802d86e76bca64beca2ee759dbe413887969e1c28ce3107512996ae07872a314dc4aa6116f87506f44956b89cbde72627feaea944cfd29a99ac79ba93e56abc8c0da18bbb244bef961b3b285bf4737e1b872cb7ea854e108838eeb8a39f78d7ddf6578cfde7cb3e290d0f9d2bd38d1272bb3c73e116fbe728d52e4426264f1ee577b980974e8861ac7523366231748984b79f841c4abdcb8e8ed37fb2c4f5c479b65717c57549b4e597ffd1f18252710a1b3fb2ca1753a4f8f410e380d6908704d1e1e1ea6d02890e80c09ad041255292a9c86674066c4dc772b9d03a12ac5633540a95bbfce65f69d3166c75f7aa1a1a3d0684ba013a02049868319a935ac583a0b7c93f61089438576e030086df790cef47eb5902fe1a9f61b5e921641ed51e0d8502d6d01cd2fd4a3b6b3c5b4ed4c32d039408e14731645ff9b8bb415f1f18d7f430561cd3105120826f0f86c2b445135588a53867e3611318a46098f7bb8f41a5ad5af5ba81aace72ccc064ee808c34bdea683196b9e9cc4423e31e0e8311d0a404fa1d54663ecb9c82d7c23d6224ae92e67f1971667e2696638b6ccba352adff85bbefaa0fc843c4ca90554c0d80fa75491dcfc0a303398865dba7371dce1b454b4814c9f797e3dcb3e72030781554fff82059efc22678cbc7fe0794cae6aaca7851863a5b09711164965820d254ccbda1afd108497a13cd8cb4bebe79acc07fb87cb8e8554f4465712e08c76f475d7bc85f96b3862a2ef24be9402fa6c4ddf78b8da391e6528d670c6fbb99773c3035f8777e384d37a876ee5dda66e18443b75ffddd0cb55f097d13347ddf59729458ac184e3e8abd66c9a8e51e34c7a2466c6fed2f4341841df960bf7ca81cf6ea65fe98e57e99fb04fdc966adb32802cc113e4ec22f00c5911f02274abbf07e63f13f778a4d4b1e64005668c6d71ecaa4dc51a5c98829bd82b5277d4a52ec390dda0fd6fb0aaeede4fcbde559023d26320c4923e1ab4d38d71e4f443754537ee9e69042a84ff8c6cd9278b4a07efc5c87898937ceeb8a9672046361c8f40ae348530783c0ba9297fb6180c33288662825761dcee667237d7225441f76ab602b3ba29f675c8f3f3720e8684882fc35c1b490ccd5e7deb0fa07ff136a7d1ffcaa74e277847f3f3f4fea010557bd3e91588cf66a5c532ec71c55bee51d0427dfbfab2afad59cbd8045624e4cc13fb5b36cc3cf8f2a989680c2cd68f791e371c95ae06edc1e0d44489dbb4dfb72116c4b4a27487c7f51e52bc09a54020c708589049abd50b840d2400fd31067bf5aaee5463b35fa84fb876502ad31229c29d1983ab3df535595b0636b899064adea87f15559181f086c61fb4d2554e1fb8d07b2be1a200eacd5d5057124a37613e73614131ba4619dce8bb444110acd824a002cd78b403780b553ad58f995f7e19eebf77c9a2c7b9a377f825f897b432f5dd96881d77937c069acc9d914deb3628df922e8e643265fa2a1eb853702521ec4ba4b9198abae34ad891394feee74b6b609b888b4a85ea2a3067a412518144be5a742274fe9185a048d27dab635304d9e3a569f8e46080b2c0fff90a1ee41a64998cb85ef2406b9a0460749ef15abce725bf4878a49d7bf631391585e7a4f76c1be08495e59c5c4e1666309ad2498908cb3c00742c51b602b48def18689793b9d553cad286a3a858679a7db4190bd0cd1bcb531afd23cc91da77ff3884b16a88a9480393de4341d858a9e08573b5152bb1c718fefe5e935f12122ce513f34d1f91d46f4131a561134d36fa08274d0687d10a48d08754415833336132023d2bd1731cb2807d19dbafb9e590a9cc8a566a73a04258a611929cfe438d62a64752960fffbbaa17fb2e28e74ddf61d3483d78fa468520b5558183253fddbdc655e7044005d7ae7ffed2e4ef245c84458ab8147a0f30fc9d3b510a2906348c4f63a58b65e8de269413afb3adea529505c56217b5153a7b7d4cdc0a2ffe4bf7935471491c1636df11d047f8a5cdf49674987cc53880b5235f1aff199265cdc345f0b28c6d116ecb15efa8f0535c24722c092f9cfd7e4702d1b29fdd31fa6661cc85d81c35a9af1e673572fec913ecd9dbaedb07722f3ff8f15463d91c3f6b0e414e7118324b95750598ef9b1810800f6ec3faaa7131d5fe9bd154a2041896e4dc6d003dc2e1ef18b8c9ad85fa31c6f95696f45ce3b5d31c1ee16391c949dff084bd254cb7a320b93fb4ee9956a1597fb8dd8b0ccbbc806b522cf4576a6b9301436c37bf098f51284592363bd5fefd737f46a99fab0b7bc9e2e1a6dd7e6f748453f8e8799046dc034c7e9f2830913dd4d41dfdc5feca0661aa5772cf07fd93b20cf144488c623ed69eeab11f68e7ef9554dd1aca40e9d8dcb0361e41b754ffc73375021e7bb2d7886180e332da02ea8db589ee0e4d88e592c5e542ece315659e06c55bb71117df0ffca8fce25d9d28d4f896b13b3065c01fd6b6b082e9569d6df8c16d426b2762eef6eacb1d48b0a01948d92b6183604f8952a21b278370f5d61fe9408072ab94143b40b1e1c7a264e3ad0c14b7170f322e6ecdd9b1c512bf9da3023ae84be8470fa026929811dad6d7669feea23f20e4f388a953deba7d89e484dc81bb09fb11a171c6a35c36c85e4bc66e0da45016c223f9aa8d19259b5a07e32ac494b9f7de011e05116f2cb1de17683d03ef94d911f97bd0209f2402daf334fdcfd97df02bd58dc00300b52c80b7455ede789f9989168f0dc389fec1a600be87eade268c8be3cc52683ed7a58a66a13a06a3db5ff4c97135ba3aa2d6f0ed5a7797e4a54024198cd7b79c5bd0d7dccb9094c81c5f5788f350f6bc5a734432465bb1d1bfaf37dbaa5089e8676c3c7cbe43d253f591d4ab00826c75fbf2bfd662322a70aeb40641e7ff9bcc39731c22642411881e5e564a36daf1a45475813170bafe6dd838440e872bd0f3f9da02ac321023e580f43561e85f9dbb02e81ffb3f5c0016b39c5a9e369000eccec90356a606e905ebbb574980ac9e5d92d368146e80d028d68ce28e42606dc105abb4f1e88aded4cdabbd7ce395b19bb45c9edad8e9786b09f9fc4f4d5c6b9067e39f23b13da6df7fa586cd414a866ac134c59b97c90a75bc4335b62afdcf545d364c35fa29b59b95d73da0bc5905c9ed118ac1b4fa67c7c864c1f7f1dad7d163327d3b447a4ad6d066bec372a3c407fa2f5fcbbace051f368c1e0aeb95c3dd75b7d4f9b053db594ebc78e656d0ba101213074e2d70b44265af8bbb58b131fde7a66d3791b8ff132c2a28635f74946674794638c65b4c94843e669d4e41f3ac82bea7bb3f5d3fea926bb32343e0b729e7e7bd8aefec0e9c2352362dae01a89b07cb512e69efb2d6b55ff91b314ac98cc2462d3e2abca35b8822a1cc5ce057509df87e098c41d4b4272484cc1aa97d6e4029603f06e91da47c1de1fa9df50186c4050552bdf208d6a2eea5ed3075fb81df275314099c9a5caa8ec7b9a3e1d6a0339e7707332cb41a1b0688b227dad3b305b2c13976898489d81b5a850bccfcba125c6c3029101d0fb112a8057b6c3790b340d2fc18f7a6c55eec885a29b3ce4e863f97db159aad8ada46ec29e10a3dd1cb200dffb7bef07256d3a36ff2fe03ea29ea94ad6457293278348234b26dec30403908a317186f6de835fc4411dfff098cb1eb3eb6b6895f4c6443d92adc3117123309b8d922fdbb74193ee9565be7fdc7d20b7046c84f71ddf2465a602c5aa16df5bf64b61e7f0865daf3058ca06f515ede8602ff21a999e68458d044679f02abaaa4610a37d7649a85083b6d83cd58e643d1490bd53a738b8e305133bc4277651919788a2921a71bb6701434b35270421a7daf64b0bea96ac4d7dff2db36cea2d85c0bbfabd3b94e9118dc10d3f04a0fc99d31f42ec5109ec58c18f51f3a93cd2fa226bae89ed0e6a91e21d77dfd1a897b690deb075dac777883c992a87485a43be07b9b5ebc09bc390147bf2be7830906687d62b25aef7679500b7b9f010bb83c25ace59a29aee4a856d38ba9cb586e820d7cc89b6d390f70b6b1a2c6ebc632c4f74384f6b0ec9439d8fb03bb1492a8fe75759d0d23d0cd415de833e97b72c92c929b48006131552b8b2b0b360a4f96ba424cb16a66534bf55e6ca1b37758ff4dd9713e9fe62e169ce3db4ddc7f43901d75d16fd8cb785359cf0b5eb0b09b65f387fb391438b30e923de26daa3e0e21a25b9d0913fb70abc34107e05079199651c7d34dd656292d4a0f349814cf5243f6c4f4ffa2555595ea418bdb45b92b1b6452c239eb479a83479f2c80201e88e01ff6ab99feccf61e59d42c3039b3ed150eb111d7a56c44c844d8c36deb825612a4408c3ebf0d3f900ef778b2aa6002457c1c1977698dea2f5efd2e0d32cde29a922daf0af47842ac0af00844159bee190e85e05c5f4bdf637c05edb80f707efa42c169d2ec9176fd3ee312efedc6fbca7a99b5673d50eacde3cd5898de7dfc63cf231a05e9f971d60c8ba44df70f290a58edaf73ff502e46ffeb8ba49ecb27a4b983a1cb669061a7c89838aaaddc4bba0ea3156d8ca2e47b59e4d8ca425197e94becb7c9477dbf9d7db1349e504aced9422950932ae4f24589d01007ab1a3039a84b074d973c3285fa7b02bfd8de6535d4b996488c3b2047c10485421d6c276d7a514fb49717d819e38a7f57c1e7f0d74369d1fdd53a83346ebd70bc4f08abdb7881640f37932845ced2898702bf0e567c564bc813af89babf4cc4b26f4a68cc5072a1f94a41d8431359cfab347e3ffce4dea25530cb3a2857a50be004d8f0d30f244916ea6ad934ba9d0b6b64719e016fd828332d8dd0e7afb80f28f0822365f78879cec0129e53ca56c57baf2bd1986e907d8ad50f3d3a2065376d9bac29a4a8fa21bec8aae3c0782b3ab847e259656167c8dc9042f018a378f8e1c0a1347d7ee6f57f01c950cd3873b209807720c3389cbcb93397fe4936d0d3d9ba42ba609155a55e9e9dcc54ff50407c24c559032f129009b96cab7f25204bdc9806baf1d3b55a91a0b4ab076070391a240f2c7396578bd8bf90bc800df7133b01e3e0fbfe57c1db074a66a31504ac0443f60e64a0053e52fcaef291cd2ae2b0a4c56a822f821419f2ec209d5276fd6aef91cbfbce92eb76fdc87300acce233c84173f7499c6735bf6e21fc69c04fdd4ced1c4bdd4e91854c439f355d79faf953b03a899a2348d79750b331896b9d574be64dbb0f3459e4b37b0828a707d5d9af3e41b268ec78a412de8915af639d081cfb002ac51025f3123de023e9cd156e6b848efbdf2687c51ad61619480744f5238aedd29a4bf2e55f8da391c1438864977af320592dc0ecf86b1bc3402c7f08cc999e686471a79f931e42540b60e5f387f5a84ddeb34882e85f2199b3f39495a5c91ce7e0266d5fe6a09602a304d7672340e0239cb080914ed46faaa613bd6d763168e8c81309143869bdd352bb0c7eaae1d2f66aa009dae70514799725161d9143ceed53e92a6d61d48df8ddc6530c83556688eb95e981b7dca62b5a024fa7fbfee288a3884508f435d447af3d89967ae9cef0b81756a2739b84b743abe39e197aa78d325e774d41ce2c7635bcbebfb5758bc5948553597bea3ea59ccee82570f8d2912f12e4e1ce7a451ed133bd82e1974ad4d413ff75b5d4f458ce69b0a3b27e25b7b39da3d641906c5bf7a3232de6f97d4d3ca4906e0e2d6bb7a4c4b7beb4281ab10753d1eacd94d6b60d32294f260981465351eac5c6f8e27235898aa5af320621878ea9af945e57164f5bda0b02f2fe1b1ba7139adef0e98017114f96ab6e5d38071de22e6ace266e44e676fa4d63787e7df1e35316e56f086f11ccdb1a95a826d5b9cd2558d10cd5b6137871050d4ae35837a5aade5f749a524f0cced23ed464c004aea4e35848298644a0d0ff4aa82f3acc29f859dec54c5a78493ec5c1eb0c7add8e9d6ea151bc1a6d096b8cae2c3568c1c8574bf4103bf1c8bfb50e8a6d412f262391f2335a538f8c1d077521384465e4f227f54f5fd5799915831b87a0896c6f33411f1eb26702bd0f082a01c0614018cd6e81c65ef663d43869e8207061e5bd1e3f23cc143e2627181587c33330e8b8232aff6aa0007bc2e92d8a4d594a8dba01b62ba1dbd318991fa1171552eecbb79be2f47eb937f9e29b9cad9bf91e5eeb5867fc13b8a274ca4dc4e9a2a4a4775d254875b0572b4538090107d6a1e52237896d944ded21d8678886f8a474bdcdc9eb5894f775248b1e4f915adffc9f69b45cccf324fe358ebe0c99deac2ed6baa66d20f97a0360a00abe5cbe9af3086e4a2555ae81afaf8008e7a91c0a3afc6bf03f48f15c706d3a6b9e4f2443f8bb16aa542d401fbfaa5ec2b3be1b4b93b3729d14aab6fca20e0fb802f6f805ebba0d0fcae929e35a31f3a96b51e9aaa9918e2e41abb57025b10e69709d0e4598d8644647440081bf162ba2c664a5b22b19df51eb8f4ae6f8910e3c61eee09e33a7ecd489a524ab001ff43efa788897a57173e78c0246ffefce2fd2942f0b378214f7faa9b30a7c476f47026631699ede9a466143c2ba3e2a5b46498fa41a44cff09dc1d3e5672913e705780d1524b8ca091c447654ca8744a89bb17919391025d09d66d87d5169c48bda6c0eeef06d4e8b83402ade2626c83e3f3025ef41822e1b3353feb1ad4026a4e292b7740ec653c285bba31405506bb69c5b2b110bbf9ac7485a7b5cef0169b6fa814d8f378de66ad4d808db0440b1616c05c2df9a2ca6a5e2d944adf5ce45e5cfc2504db7b88fccfe0f092a9ea52adf7aae3d072ffcceefe5839bd06ff86142cb54750d0ca80b8cd99276a5c04250f41c04ea11f2aa0c8521a3107463c6367afd2e095e8133d4092309f78c765048d90016da435e829ce9a62308f1d96284c9561c349d903124b29261fc75372c19a4797652a0b5719a9a957b2d893b7b254463d6d1fe7a04d9d8315356009afb38c9c6145c7b5eb51eaf31d9f37620c5b31418fe9d7039038cf7d1d5bc8116f0fafd44337424e23e60db1737f30954f368533505996cf9f5b708df355962c038129ec08b794735771c81b3f3da55a27f8672b659cfdb66c64b37f8a8b99eb556a402fe29d1ad97f6047b1fd0392ad62d3623455bff80ae6d0ea5fc4b969ed5d6f8a9d3abd4148abd4db352093e66781acf2cf0b60f93215b8b7b8f1d547fca41cb7418f430c17c6ffd792f61e5220f1b0d4119137a2bfd3c34f46e498302feee4f60d9d8502821848309cc771c5a60a9f07ec40517b9c598be7d0f93891a5156510f3a59a6ab94a62b9775a9ed5cbaf1707461f7e1040d8e0d119e5d52182ca52fd44283a583aa4804ac28f9571ad801ea1c4570261b9f72e056317a3bcce50139ed2d0cbe3103bf025c372c5e9e0c2213025b60d2c73e97fd5b4e1d00d000831012f91f05fd785196d9d9f006a20cd726aaf2a4fdd3c1f9087dd1ca74386f12347f608566f0db4e0e07fb4a3ba0c281e892f8e19dcccaca67f1168c0051f44b16c44ac3cfe326c62c7fe2d0830bb39dc54831634634ced6b28d1f0c6b75a38e890fdf5bd8cd1b01c8e0d28e2ece6a1301393f8fa714d944be55eb8e5bde3c6ee8b6715717a9ba1e3f76288c1f23de1c07e9203ab35e63a9bd5b8cc6bf84898dd324a1669ee90a52a1c3809d8889336bd36e3f2ba554b03f8624cd16252e000dd5f2e3ce5b46fe653eafc006a25d55a788af42d530cfba566e26560e3fab29114fdaec2e223592d1559852150b41766bac46fa89b03f613f71ad67e56b112f7aee5a5910158b42abe877382eb206982e7f721a1e4abc476d56e677d4c334489b8a9ba05735724b4e35541714133220bb485d43e78a2e139f5f891ace6af47af1e2671b04a79d321bfc5f0ddf0a75c74118c61bee829764cb179bae506063b4574dc91a808ce506baaa64971c1107f1236f645b168d003faefd04cc7b67fafae7a1f4230f6d336af1909a6ce696ce3289589acab99c9fc37c73ce8a2734979173b21a5b758d4fddae0c6728236b076f892608bc8140c99f56ccd79bff23649815139aa0b3069b33f9b203cb23f2944a97d71a6483528668aef8ad567981aa3563706bf32acc53eb783c55744c9ef871b426fb5775a2be575e7f884be9554d80ac77e37c71cbb4fa6801d7144d4298777aeee18cc4fc600388128b97c767f1d438b1b560e04107941c490caf2434927d0bce452af6daeb50e25ef12f313bd249d9f74d47b6b8af5f8715f59c1a4bcc0835a9f4485c656fee5af3617dadbb503c1f7eceb477d8f40ef15966104df4b6848956827796190548e2f578eb8fbe5b1f261e0a6088d32bf3b474782a1b7608032c2fa06227a30dab18dd1f93f049a09e1a517b028e7652a15960e15e7d1ff53696d272333ee506d59114e3a85b4722261b8e8db78cdb7d585700775487e061819f6b30b551a8c0ef2b43d20e1d04fd2a10d7360db3157fa83f3cac5bfdddd30201703e337f5d5ad2f18ef4ad71650f6aaeda0fd47806462a4e1bfa58eb147c2ce168e6fb27174560f5bbb900a14babfcd205f91b5b8fcde4fcf3c8aa60c1ff8012e8d93c78fc8b07911430e0f0d73d1bcc9ab910c4e8ff7574d79dbbd0e7b7c1e16ff713d441ef92337246c04ff0eae950b5782ab2d5be7a0d5a5d36d0a951ba1ed9aa695bc93783e3c4d97875c24c8d27798ea66be9310ec7acc6d5b3506aac0fbb0090bf3c6a77d049116da2f603ee3baffc5c1bf195e8124440a2b29adce65565fb9f1b959d1b4c70dc824afddc4ddfc553350146591b3bf9d9961a9745d2060263a7d7307f68e89a0bcc3504a9578b4d7e2b19367bdf759e27a5a3a598a5cb60532e86a08aefba4c079dd7f26d321bc925cef070527c20a5667e6c942395f5e6b870ba9b982966c01a4c7f66fff4b8950d54da866c78b81e2e2f208f7dc346d67c7e7537997d8d31bcabccf40c84d3c4f0e159ca4f94aef9461d9165488c3f3a58b8379963698a700bc049cc8d2b1570b97df5cdf73e8a6c27e1bee883cd4c3fe052c0f52795f69065d1a243678acdd52b8c77e6c36a883e7a23024b06114727f30bd4be83c473cc6858ec82417ec0d6ee52af9de1c53739b191c0c87aa22d5c9550b1d338dfe0c9e5749461cfeafeb265455a49255df86dd8f27238539b46e974d4963ff593320a3bfa7f19008d87dc4ff467ab00e6850c422d22a6a190bb435af51b3513b71c348715ddee2f90d6d5829365b0e3c96a7445e8675e73259f9f8d4c190cee8d2eeecf9379e7c8890567b0801c4133352717e09c13430658341105753cafa4b8eb39977191f66e1159a1fd86d30f378ec830ff35ca893c0b48565845a6c4606baec8661494bdf076acce24de97a9326ecf4d0a2ff9bcfdbcebe9e1e804c5a573314a5bb8308cd9fb33adbddea00f5d64831ad641807454ee692b8d14f8db12eb1f87bb79243db35265f8b2f15e851e6b84a15284d4b0f21a6acd7d1eb28dcdadcb496461223b9a190345010219324d312107a15c5a418c0f3ac208ee023936f35b1e07028183e3c0325ec039915537a9f6b7f54298046adedf046bc0ae2eea7bb665f942de2f49497ea8554f395a3b8e545b05fa5bde07f6b525024fe6c6127ab0c76ab64172525faa5824813ec5d212b7f313be59a178da9e47f4da496d68c37d039787572a2efd4af8bc7d6bfbf3f2e93bfcaef21d59fba9b175fa7544f367175ca0ab91f75dd34e137d38aeefec99c2d98f5787df2f001b9fc557c9790eb099fab970d303404625a0b218c82a97dd546c922765d3fe152bdf96633022820b1ba528274375d1515ff05b39d4d683d2b832d5d8ade432be1a4ec6f9ca3dd2edaa25183bfe11eb5ec9cab3150dfd523c90737ae0250dd62b390aa9a038afb46b2e6c971770c3dd5b85543725db4ef93ac90ac3adc18ae2ef04801ca181b88511f7480e6d791be069d61a4ef0a8bbab843f7474dd2458dc6c454bf7a29c035b4be0929ac73ef77a76d7dee5ece62145ea9ce82221b1ac224934321445f292e7cdeb63b4c124b0d27be16bb75da86812d792471ce392cd6348876e3346b479ea4d74cddd09d71254dc88f6d2f692d6e0e85ec1fd9d3b49497d78fd6f9e34f08cb598d6dc1726a12efebed6160ba78bdbffd86c455fc3fe354c33eb6c4a11f051a365715d1bf7413c39bc39f9690851f87531571400b8b29a87b05d7b3836acf304ff8e59e79d93e38ccdacdcb2709ea7cc50a9eb93286cfed3c41dcec749c0c1592a9d153c217ae53686013538594b0c9917d1f7b535788ee972fd628d143d41865fdf57889d6e42ea70fc32be2c350c936556a91d16b2bace536b65881d319362f3f63267fd97842b5842196fdd42244fc26bcfa82abe088551b63aa623bd2842a069ec70b66a1f4200bc47974e4701a29d596d26bad5ea38d74ba5a5fd437e7dcae1f6a7e57fd1ce3c7643fff6884777ee79c6a7e62d1553a2f5d295c21f83696ab7cdedb29065d23d6b6642a6e3b738de2cbbd84a1ef7800ff72f94a4de152279d78a821c7e9e6f23f17e0ee951adae76a2a44415e4114079de211c2c5980fa05bf3d8d714ac93de3a34ba21714de9d5acfa8d265cb6da798793debcbd84192521144649311b191b2c5732b7ea210660be21e44f42260ec7f1450847a176496cd08e9d3f199752f069b469c3a0795599679cd41e1ae00d1473f4128a63f3271395eff9380cb72be427fa3024c9eb5d58e134462b1ffdb37505b9322db3c1102c2454a9cb16b0542c37732a07baf7dd2584a1f2bbd3e8ba07b7aefbdc7c14da0cc7ea680153b4703261bb9418a7d44c33b7b94c6757eafbbdf9884f1ba588827b0b61a6eb9f6faa9b482801c4e03c261ca4b2c3cc538a8f6b5dd6d6344c9a4a190e8cce59ea18c20f7a5f982bf3d4f89d218844c9d4caa1191b792c12bfb64b356399081f69408271ab39be720218ed8c1c3ebf9c3acb28b023da6c25c72987415b40d82268d65ebaddca20257ed664e6178bff5cacf90285c6534748e5d1f52cd3e87de9933674298abbd7b840ba82f90eb234f093963aa0a54f4982662ee72af43752028b8c484383abff51e552a79d27e7bd0f559e42c21fbf0a6e0eaf62b92b3d9570f4dbb3fdee54f5ef2cc71e25f770854c715874fd759eefd2d0edbad9e1e0adccceb7a49cf31c044e56ab59769530705e37b0a7db36b209e2571214d478d0160163bd172440b0b582ceac439d6a13a21e3faa4a6da12e4604b029e9126052f48f7df9bf6a858890f46404709c39ee681ea1d66b580b1abf7a765733325bf5b11903c429ea2630d74da972d32b6a780da684b59bb4b9bde204c3115e7a9488d619782b2868595590ca628c627e5f64f8b2eb75240194f649dfc21d7a0de2833eacedf407a679258ad3e774aa3d2b89ae5c96a43eaf73222b4cf8f88bdbfa8b3f9bbffe1cbd0ff6e5db5e6f788b501756d05a4c8216a47288507cdc672ba17e4e83fa579defad1b3756ee3906525c01694b3a5c31d97d7db86c102916ce91452c3e40955d7bb2bf6e7a9720a480f80fb97a3d6fdb71db113c06ee513a649a2dbc31bb1f2ab1f69f1c05e5c8adcd4fcb60f444a6519496bdae890fc7da4a3ff08cef3a365158fabf302302628e1b5ba0eb88e656c0858ea48d1a95fbf95eccc5e730a17aeb27132648ab777140f26dbec9df1da76b107b5aadcd911e256701711fbb8b9bd340f71cc75c8353520a3429d142ddbdc31c7f4b4dd9d3d5e084ff5d515f2b89be0cbd3f1264a8b325adf2c0404828b49103ef04b9550de24b7483e873c7486c89e2b9e7373075eb8a7985007bb90385f8caf772e256bebb228e6f1f4bc1889e072fe6cacbf6c4b9d8b14d847e40a849fc5120ae5508ee8a4ebc8a445441481e7af76773a436304a5f17bf07c306c51fd658ade837c16475a82407b1968a889abdbe2d09c971fdade7e1ec0fa2359f61ecd23680fd36175de8d75822304949ea3ecbb02add2edb55be48424284bc2241879a8895cea4a392922eff62e692f7ff843a67df0c055cb56d67e64ed6cca78550f6f3d4a6590c24c612ad014a89fc70e9cc39614588f49bc623ad1c1fe805437083208e5069ef74b34451624b74a304c9053dd86bf06d46647d372d27b363bf6c401238c2410022879ea80f71ed178e5030bff8cd8dcd611753d620ca488723d88f42b33fbb339a2d1f422c84bc684cd11bebf48431d033a3915e4f889e77602e56464ec4ef5bc061170dfb4c7cbcec493a295cfc697a2a1e2e6293fcae03b282fd55fc5d1bfa4316a0d77fc08ca5ab7321405fb8b2fee40c47f48413af77ec73d964769b35d1e403fa7675c7bd60fc1bd3ec36fc8bfdf82728171d90e536934aad8152d21ee8e1f4eea049c2d74f674c5fcc5394f00364d9e9444763002bcd9c5c93efac81d20f4ead53b2e57b7262dd8c689f5ce70040e895e71df131aeba316ee02603caf76f72e1e3621c9026f14a84c1071a463cd416acf9aa3086da609235e5aca5b9afc23fc9e371f526a1a83bc9bf79614834dc3952c32f7b0342e796ba6f05bd3ae866baa4852ee9ee5ad812fbdbafce78a764e969faf3e4db8716b24410eac76b9a265c23f8e6a9c125002298e5d575fa073462a43c76edb6c200c56498d8cfedc701d4cc38ba169ba4a17c4a5d507bdddce4db943c9123f3b05c330b6e3d6147029ddbff266f4bbb0dc07bde229a34c9886fbf3b180210b92d8c6da707e427098bdc5e23668b89e53db637610f41183b889029e8706fe189b64d537be3c81fbabfeb4deddedbbda0cf1b90f492c5d5fec95bc87d712e4523f091b717d64e9a75c384a81e2ac873d10a7c5d5f058ce7b07c4d1c99c6d2ae2c1c5dcc3d2791fc95fceaec22ec95f48d2086b6fdab96f67fee4f2fbdaa4f1275503a06d59e8ca4d262c39d3f7df8ef3828fcf7d4a59b1f3f49fd901db90cf88cf4fcb2dfa30f289ce12816d3fb57b30a09e8205728b99b3eb0053ac9b79e3a35611aa7b79e13144956a0101a41853e51f4db4fad71d0c6fb79edc3c56c46170d99099edd81ad007b9d772168bc04cdaf65f4bcfafad4cb0ec72ceecd3a477ab45c38df4c3e89717042b68893892aad2a30a1df0a6164f2a87633cbfa9811f652ef6ea6b7e3e1c8d4d056dd77047b8996bc1ca5e77529391fced80ba0bcd2679653c449876ace3fc27e726c5ded33484b6d6712d9296966c11d18294f65c6dcc8a45a8ab9d481eb49c72cadf2b77b7915c40e64ced95a64a4c01a0e01975f8146fb2e5f1ca49aa4d0ba7b80a5d53f4ebba20c20e1ee74c53eed77be9496050e8419f613e3b5e7f7cf9a75a1f6fc4112fa9a2c7ddf886119f0ebdbcbd3cc17acaa40e51937a83c16d9e16256f0667aca62d4b613308dcbf9ce1b0483c8a206c0aa108e7b1606ca36bfd7c001d799aba2a72500972bc3b435468b29cdcda0279bb8e311f38c180753748f9760f9c0784e7da0189c4de8cc0046beb9044e2da1933507ac726d95c9712130a7a04191d1320459773b883f334a035599fdceb1ce3f9f4990590528887dfe09fcad08c50912d43fa2541d517ee0a7df09b12d387162b35234cf25c5b02ef69cf2473e70d1a475320f58f7d9cb7723ff9a8e81a5485bd8cee8da3b0617671aec252404048d7da6e2d4d18e30f2733471ce599ccacc7259804cc3df7f1a1002458d52f7b2055dbd792ce4208f58a5d0af065657c0d96eb82c3365c54a7dcaa3e6d46b6101d73460c1c25a1c58d6b16a7426f021cdf20c6defcbe4c0bd2924df19b313de62ca30da708b791275ef727febd0e705080767e8b2629fd2a92e209721814c4482355eb03de0ad17cb5842a1c14e938206fc8dc4071dd8b8019fea583e313fc55634b65529f573ab84186f1b3447fc63e0de009cba1d1b267ce9582c92cae2eaa326abcd62d33a82b000d89f3b3ffe4d00749db546ed5eb2702b38e7cc7e4c0712c0ddae52733fe54f2de4a4a4760b92fed1dafd0dc79df2b2965d960c29cda6723d00f9fe255930ff920dd690b006761eef1f98e75164d64e7174f399fa82ba2b2edd9c6ab43667fd7b7188d646066a3a264134e4f563243b12a4b08cd4fb7467999daab9c92830da59efb5f901f8e4d23444ec74817b5b27f80449028186354010d1ce537bcd8ec5334c6808ddf75dc5fd2ce510348bbb404441da6ec607bca1d93667ef700ee960c4cc03114cd836905bb0f5a955931fa12a7fc249fcfa420a34e043c2498aa23c4118e541c7471860053e38dc5f8bb15da6aac2f838e07ffcc9c78b991ec97525dcb6e44aa9a9aae35819adcf43014e49e393409401055a995bca64102cbe0eb302510582d23bb6e3942e15ff8302ccb554fe6f349e14b77c0c111270c8b38a971ae4231fdad8763a265c32414c5a9cc7eaa4ab501edefae0fddb1254064b5b9bc1d7d4ee4d79b6d14a0237cdf4f67955f2652aa3588cd458b56dca2a939a53f0b924b4f5bb6ea4c7ae7ae82c0c0b9b9fe92e29381ce30bfdafa556edb07e6523f9bd998fd60eafc128027f74a390a93245519480c4bc309b8ead427b33b2d62a99e4099c9639d39a6840ea0007ce46a7536d501a77a4eac5a67172dc01162c4aaea0de91c7e72cdf5bd92006d1a256253de5bae21be100d3cb5f7195751c9c90b85527be91cce796add9a7c369d827ed5bdc301bc39019a4a5172e1656862d30edc70683f1721359efffb444a751bc958c3341bcd07e0e6225da1b63c6f64d84df4e7874c78cdaa56019d2e6fca7bb155c9409ddca3eb006dd18f1a007482d2bdca110a385c61084af693021bcaad1d3004df91d49b8b59e9fe9f121fcf6ebdc898aec2176c71b14e6b53c2bc6a1bfbd9783191997f08048af79305d4d7a53f504ce22e5f9f6ec3c29574adb379d0cd30d5df8e6568c39f97fcf749c75d3aed01edf35158ac7f0d5ad59393f16fbc67dfed5d4079c4a94c09604f1aceefd76d51f6bce25fafa9f660a2ce6f05de4cde82ef2fe1801423372aeed90459f296be428a3fc869bf48a7a07a16929a8c142ae12bfad911053c6c6180a09d51b794157ef6ae48491d62489383dd5e705c077d5c44da1f9f56a0db0c9f2ef45cf8c71b9f1c6f1ef5ad05b3218f8592125fa4255cc8797d1e4b0e33f71838c4bf5e7e5fc691280f3d7cae8b46791503fbdecd934196401cf6e1e2ab19e2a111fe54e5334932bd96ea9f9563d238dd4fd42ce70b0725ba9522092c356a958140d3824316da8527c4a5fead15321b72ef8865092bb2d9c8b40876f42d6cbe3c92dce084691cca6dbd973bcf8f45cfcf0ad5576195dea808e29ddc40fa997350008f244f5ea70cc7f1e497c5d94e63797ef8d72b3c1a0aaee47730108bc3ff1b27741d37592b669e8e0b6017fa992295a2eff56044d4fc8a91dc514d7f2195e9c6da7830529507c721d11d79e11abca2a927167ebf7cdbaab2f5c79c63396ec2b65f843f37402aaa67fcd5a81d00c8b03f9f87b75f6a4ea57a9cecbca755e84fa53e94a42a8c0d419725f0a24264df4d004f2a2e9530da8105e1034c5ef0af86b00a9d31ab5ccb10f87fb05ed3de2d49f2643b8769801608659f8029e48f6cc3f19745ecd06e8f3238516e8c3530833b61f1cef48d493e5c9f69702ffa86fc21217ae09c2c6b56bcc3b558bfe8f33997da77a13df0752b5780637d27e95cc20a9e084cc51cde971f6fc3fd737ffddfe7afdcc72c73e739bebe831730261a7edfcd4b48a1ecc58d320786904f22117187ab15d9687e5598381187157138f89b38f939369f79c975a30d8a2e0861368b85c4dd10498cfcde019739e6960ebf58f11508cd3d03a8fdf4a04b77af6a6f5d1956c97cd68517fd1fa205fd38bbe3400b05a2d58d3ded84166e86bacc93a7844cb71b66dbfd78bd4c4e2ce9230e733894431fbf41c931954923e78abb44f525859973a8cf55a1c96d9eb4980a86632b8e10de6532d65070349dd90bcf5b6f25c0d3253f571869ee9b4cde4a1726983bb445d83b8b484323a5dbef4c4628b39e203b785e0b57999602c2b33ea12c1e2804ed8e8c4146a5da8ebd8cb71ca2e96be256dcee16a5cee5de56d5beee6b6a46a4d2022b5a0e2d2aeb9902b503b47e898e2248a1ae0c4b3d679718d3b7cf8cce9f493d410f9fb4cf4a69f4912589d26cc917f9bc394c23d61ff58ac3f353c7bffec3a11af80ed9068a7b7bfbc598921ce84ab5ad2254ccac8a0e591018bb26a012a5292e44616f6706f6737a27a6f6258ef458595e143f0a2ace83d43c20b9c4bd8641c3310b7657a4295549022993123c3f783de1429202d24fa65f0d897af62bc92d14c4633caf9f92cd00d4f3fcd44bc81fe81ede5b732a9000bc0708e825b4c02f5e03f535114a0e94bed4e5fea6c6f9bacd1349bf8ec6d9c9118ed08acff87ae8585e96ef6572a94423579d5c94f9b4d4bc316b3694dd2882189d4c9b203b6657c26509385aa91ac43b3aacc23a9a348a0a41c1cc0af473a2e6108067c5d0218385933995f105d9fc4485b38e20a82c8a1ddc4d776986bb07de59a83e8428d9d2777d47b8c5b02d8311df3e09774a1efd2c48f22bde136d85d2ae3a6753837fb730d8e41842a2ee5c6b51e4bf34a9dcbe88d0f949a677bf68b3bee0033db17b3c984a4cb05d962e9c48422df2bff2b466e2d195195fda86c49548ab9a65a4d4304686b8c260411aebaa18dcb8c2bfd9f806774284e1914cf1bbc3061f57759eda930968174640d5245d9dea11538ae2cb2fecd21ed83769ceed4641b4a3b4964905a92f729a90119c17e58f6e50d2718d960959737abd0aa98140aa7169cbd045e4a08f34b14d88db0262ed87d91712441e99657425c640d02c1b9623e55b8a4d44b003516e5f36b4644d006cd68b5ff9c2eb01ccc3a045c486d7592ccc41c551b31a98b374ac69f95e8ebf2ec5c054d2332cfc8efae711c75cbdba20e41564f58a6ed8e6460a736970196a80701fcacbeae706a647c319d6c4260091e4d19e9123a8e02c3520e9599e966cb77a475b55e7026e8d8e7e7a0404e09c40aa21fe97530a572aa4651f26aae0b935d9e884768cbd69573760db31d3a758c7fee77d15c62d18424b4080af0c258ed29cbfd75fae4d7029dcf43fb5c3d8efa01ac4db990c5f97d08615973522b10faa50bc3109804f99debd31600f3e25f8de077a4142965cca0fb44b8d6477f3ccb6abb0cccacc2c93920774d629c40685eb33193e08798b07fc548baa6c770b9ff48f82815edd10f3704e6b7464c9794e41bd24b29b2d05472e1a6cb6b28a28f096b4adea735999c4c9b8021be7506497935880910298bf2b8d5863162d9295baf3cdbfa261ea4c2707674ced638dbfe4fa5b66d6d203711f05e4f3c9ada3f0669d51d80c1ddb9a3abe7979e18024b4a8bc6d35e272e0be720a484104ee096d044dcc1aef13ed6671c285b40cc40a31190df9e243f767dea3c3e88ba51da04fc9bd60ff05555ec28568283cc05aa56986d27b7835860bb4003aae99b1e77291de9f3b60097f56c9184f91742dd46df0fa46be2d4a97485e5666c4018d7f21146b6d6f794fda8e3357777ef16b24689650bdd4caf8c4ddac4c2e77f5f49499aafc67ce43dddb79536e0928c6e19254c45035a1d9691e3164cf6e9adc3741aeb6e986d314ae48ede1e8aa533e9d2b77220ae81cd7f0bc580c32519dcb39e764a4c2e54b6a787206163205eb53f5ee752329a4a9c6d4ce06baab0099f75982bf96d91dda1916409805dfebcd223b8ed140ccacb50cc22794d454fc3879162359fc328f87712b939419f492fcd7d96a7895fa33ed557cd401ac0f598f6cc9625c278a8ce5c21e38e37331d4aac185365ec016f675213448b689607afcdc3da601f9c2b64d73f2da90bdbe055c873b02100691a3b4fad0bf6e863f4a213ea827e2db216784fa95101c5bf36428376e3bdcf035f47ff58b55a9121010d657e74f2172c76c4c10c15dfc97b99ab1208ccda7a0ba680afcd214ba1d17bf27d2499293abd615b15bb2d5f3ef2863a9fc540d67cfc242bd31fcf23b271058567e3cfb02449d7cca8ae877d5297c8bf7fa758dd9677c048dc76b01f696132f9082637263f7778f880e2255469a64648fc14c2295b7ab1d4a528d7efd435153e5eaa746e4fd039efa604e8b22a0e81a60d31a575434e045249ab7448bd80e0b32e7b22f5f46949f4a8cb3a333d762d762828e7fe716641819384883d50c498cc64f1993c18e40880ec989242073ab11b15ea397e35798379bf8a5ef43e686f97611603df9c85cba179be10ae17be4535b57ec9e39aca48e7db06bdfc1684070d6d2d300664d7c9fb18ecf7a342d4134ade31a8f270dd3a983698c4dd29abb03d90b1a621829e91c30e18abcf36add3ea965b42471dc3e0f6dda34fafd8faeecb1463e209a8a8897e45af60a52416fca3a3bb470c594ef873bec1115e1e9d5e8a99956e6b631b3ab03111cd3350426393795b4b527a9c2abb78e3b7f769c991e2bf5e1d33a814ffb58f77c6f42a7b258039e315f1da094c169b5b99fbe1b84f1100ac2ba55b8760e0d084c3485b7e0e3fa0b782dcf17a299a0ce96483f4173f2dc36d2197bb97e4dd87a2029609ae9b8b2e7b41abff49dab26d42fbdd800d1c409465f08576d1dc19c3a96121cf80ab22b0d6a70768d82f1378bff8cc3d6ec8fc6710e3b095c3225f7929b5002f2b9b63569b5ba9498c2af72a1fcb494c4edda818e62abd55322811404ff8930e2f7080c1030d1bc3c549ce713c9baf70f70fbd788de5653154fd5cb5cecd0683e5fa73170351aab30a20d82d0ff60755d82d7a27005c3c8768db906aae4af2857199251f60a6c601aaa0c89fd223a6583a692176984a83c654b74723f7dd1bd3cc92f7320dab0db55d63674cbd3003865311d9ba9786396b951094791f5fef74da835e0aa8a9266704750bc30f07d17f620bf9180888863c590edabd643cfb2aa385df25cccde84d01bc9d569c63d8fe7cdd99bb76a0521fff72ca796270e1c677c5f04094bf06341ce747b053c66cf95a31db784006c75269d29cb262dd093953721236249979f970ce53348657dfd38d6b0e04cd203117f964a3c141ca43a076a2ae8c7a213ca3cdfb758aea6fe9b28c0c5cd4bfde793a98359831411b6e5d41834213d280742795574c90348db005437f263916feb0a98d613f217033fcbdf4b6e90c0ff07881fee1cf4e595389d915c92a0b078f48bd4c35b47ea6d26b5b1e54b9cc113edd3d3c658d0a2761019f7d72e4094aefca1f76bd04897710cca962820ad1490cd1241c36382a69de6711a666bf0f8d51418e3e6d4ff780b0ef147b93e7e9c094bdf557eb5262b9e919724cd8f73ee041d76d9655675d8144f6ae14d1a0cbde6cb5b17a6e1ab4965941de4fb632c20fed2d8594e8637f0bdfa2d40f26c6466a510626a3bdf401211efcbe7afa3f8ab4ea621a9069c20899a7ce74031e9be7510586fb6d33c4852be6ee03d21cc05f0f4ec2b67779807893e864e3390d8eac132c9277020045567e932036897ccf77f02d38252c16ea16cfcdd4ae64805b56bfcb8d67edba9bba4d217e3841993fa40514a05cf8f29fc328b31b1794923f6f503f5202a0efce0b4b00053ad0435c763637727fd3fb3e8cca9c57e76e944b128f34e79c04d05b27792e2f0f1950f43075d9ff8dec000325e835677a5d6ea185bf3835d3a7e65ba3c2403d27559770198862848e0434fc575d82f57a62f0ddc2e69d963ea848b57a17772d8149e84a20b8213c1e021b4792896025f5a6dc921c1d0e8878b72780dd5cd8d2c8ab9da1ca4ea53d9698af80ac5fc5c61ba38f65cb68b44fb331d4e0abba078511ff48311387aad702a3ec90b0bb6badc56259c57c51bf38ae33a7eb2f78cc2b63c14ff63cb59ce7c7eec469206bffb14b1290758c8570b4e0d58ddee1d1a141fff9ed64bb5eb61b26ac6fbc48701e23417337532101409a75a7c3a7feb1602c557a88d520316a391d2e581348813b5773da4c3e981eda5432db976cd3f2bc428efea9783089b424799c067807015a6458d6441d7e43e2b6722e553da42aa4f545998057c7f91ad52fe87d255fe458bb984377d7b613a550feb7644fa8c2b70602963697cb2f1503f363a06258aff3397e855b6219a0647f72c0c2aa2ffde350d0689abbd2b654ecc2dd83758e69b0fa9c52cd8ea0410333ab346d2b9b3706867b2ca5f8cad5f2a5c963b53e6800861e76f4d1455d14f11bdf698ac8c5a4f5a4bda21b32b55968e98806d289c5b5da8e1ddfe3cb81ea34c1117ede9d93aeca7e239793575cb8734546251a19cd2492603b3309ad0493730e73190e19477f47cf96ad72e06c31b1fab6ef735ddd35cc52ebb7a4c52f5fe112ee18f7a7e19753974cecfc71c0a11f9c49bfc68b4a0d296be2da847baffe6f0ed78929b04a2cc9f4136536d9d7f589ad93da9c2b0d8b2862a72d225fdbe5975273d388b0c45d0a1658a96575c59a2bda81b8259e23f0ba7413505a0e7cddf4d0aabfb4692cc452090f13fde8335fc42f13270691c89cd80824df51a922464dc1966cb7c90b81c674bf74ca864936a51e1e7a6b739ac70126df6b5b0df926dac651e2121909653c3b7269143a7a76aaedfbd1445f00b780d6a24fe6988ac1a157a86ebe32267c70863bc2aa8f858abae4ee234bdaf7d2c3e6a9c8bc095d314876014d77ac825e377477b058bf6f0316927aa9104cce68c96c687d2710e98f342e4eb2952967bef959728c311238998ee036ab53132d41db8ffaa34642d9f5cb989b20f84abd016087a8e635df25f7dd4f153ae7593f2a5d18ffa2c2e60420dd2c7579d5c524aa69c68656f9a0eb3b53f74f6022c38b2c569ab5c1b2c748eb658bc19fed0aaba3863a5a03772692c93c4454c67a0240b21026677620b438bac592a8f1b251f83a87583fed707ac70c86d58a6e0bbaba690ecb0719802957da87dd66072716a1c50145896b179fcc619040d93a00294f9ce426f32aa43a1223a308501ef0f69ae6eecfc6c8028790d7b5adde4b49ca0fbeb4b06fd3c710d826dee672dfd311307020936d6f1d57213f98154a57486ab72d01e7c85dfd189ea79bbb55e9b943364bf76f4cba5788ff23c52dc38c7ac4fdfb34f0cc886b0ac611bc455dcdea7b80394bbfa27eb9840294436ae29823c12bf60cb52d9c88ab6dee433e1b4641c06d5e3254e398e874105b4fcd7de5bd1ea23fbf229a0ebb201a7127fd6c2a02d3fddb3df22796f0243967cef4dab89eefb12e3c44620af97a38109457d9213a0ed47323a32226a4a03e182af0a56d5be88ee870e45be9efe77d79391e922cf690d99aa6756d42a53efc19d2e3be0723d94a9bf82eb62dceca9000d5e97b7d8951d024d90e24ca45767684a815b509e81ab9de07eeee0f12f6a31991a77c5f7371c902f1bfe05e7e590dbe8432200a87cbfd33ff08b0dc7fc2f58a86dbcb8b8e0a21e8c3be57fcf084ee36d4fc8068f8a55fe5bb6259ac6e41eccdcbad43969d086f92563872fcdfc9e7e9309a1cefa03b9098605d02294742d2ac83ef6d9546cf44f0b0897c1ac607f63192bc3b17cb5e21559ef00497425843c35efc1f1717ac9e4e8b30342460ceab0e5a810852b4dd374438f189c2a2edab032e6efb1e77791e0a981eef1a0ff566207c73a34f592b5df914f1426618ec7b9be53c4923ba1ca831e07be47ca8cd384b8cfce833eb752ee21f3044535590ca64dd5a07b0f8ef15e539fa881d49bd7b3853a1d5631c1b83f549d986c8a0f48a985bc0d19f7b596156258d96f4ae47bea121f4d2ee6d573189b3d5ae77b6cb9c14a8b3d861dfd6a95fb8c0a51319224f6af4b72ff45d576e095b4cba2c5ce630685a4e4a59d104bd0d6cf0b8a8650387024e4164734b8791e5df12ec1efbdfb13707f54c00434cd58a87ff24f8952a9de0f8a49ec3aaec68be6b65eff287013ce9eb09b5830ba03b4634cd12c92cd3a1fd19ae5323c80ff66f2f95a16579b4b68889793d94303870014874b6dfe34bbcfef027826ce2327fbfdcac99faad52ad868017b84f80db84e87dd8bc01312a6d1c271f1c3979a1fdad0ce1773d2f085ad3f13b7fafb07b26b563ca78c595dfe87f81deba7b0e246c748063df9fe31a8dcb13d34b7f0e83f3ddb6f24935e43c75e556786b30b71c9b5b505e0fa96e85677da36b5644548adbb6cfcb2e2b3b9498b30bfa0c7b35bdc593cfbbf9de8bab358ba5b3b9b7e72bec276e92340eabd3382705cfc6d8429c9a208c5097a0e6b7874306f498968198af91678d4d7529084e2dd5d678b089c347200536f983f77618fe6aa1a10c0683a96306e1308ccd461a03a28154d9bdd3f8e451e4938edb0430284b3d60b761eaccdc691f7d0786c86db6affba75954ef12ed50af9d5b5a1b240f146600cd6d6b0a30b697c5cd37483c3278de5709e7d8931de95c65dfa7cc9869a24afc1b536e2f7c3b25c15c88f8d004a8b66905040751dd3e72edb8dcd2d033c311904beb69985caa54baf5b73deda6919dba196bc45173ee1586cd556c8cc07a8bf10576e853a0932b0a6e59342e6786b6997185be7301250c0076d9f6835d98189b9ce9d2a97b4d41eadfe125cee30e26326bc3191781b152ee916d86e9a9dd8de7fc162474ed1d3480f4d8447a843a9011e9aea60be47328b14b7a49937fb8085388f1bd660dacf707875de422d19ba7be7b9f39f4b91316404a4b433a600ce8732aa9ace8ddb959a347597651c337187d80cc8a0c828bc3131420ae868cda9adfa39c33cb582f6372c5738cb5af1c792fcc62caa42e904427a567ed42ad90af45a69300956d12333fdedd4f960630d9ca2e1040120be7e00e7148109660105c744c3c19f61898e06e361fa42be703b943e3d29b07e84ba77af31faef0fcd19683d9927c29f0fdd6bb9d738d32e1b655501337c13d68f954ed10581db2986715b2ee1b91953db6b12587c1264a04fc727750ba2e8c7217ab971eb7af525061d7da14bed94f0a0438573c3843a991825653214847b145b48565b0d989b569c58cee14091a6cb7e1467cbe99dcfbf58e5d33a29c86ccfba56639fdc598e568455dda392172a7a93431b77942b4177a21c8b4db8921bef03bfba358808cec02a2db64d50c679892d1503f10c80fa96085a90482b7b5e4a7713191bd4abdec305d6d1e1d20ccb9a88fe16250e151c71df3ad29296df3bae3dad02b9313d71f317ab6ca35c543d67cc8ac0c361c031da43e440f60a2e52d66b009174dd391d9a01dca6cee76bb486e1ffe93748bb25b204f18107231ecf2bf40417e0eb299c69e463cde615ae4596690683eabf55c305ff860dc920cf044dd4e676b6576d5d7b02c07abbdc2c10254ac5bebb7b53c8616ce47d7744145cdf8cd4fe3ff029c1e38f1dbdffdfaa2a2c291301c2423c57dc2b687d49a5f8a5b5c57435d1ebb476d7bb5bff7ad42585d44d00bf580b8726f37d88fb0fbc335ec49ea8e583829d3dab7da9eb641fa6735e02ec99325f0b79c18c914665dca577bbdfa1b76b84b1d3115476a0db967248ecd74dca74856a37b3f9de7b344334835874c512919ac5f2894af32735947d54487f40433fa09968ddc28ba31846a6488f1025e3619b97a31ef67e615135e4452a5255a7d4f78b2fd263c5e2add4c168a6e23d181c3fbddb0fda774e6bc84924047e553c1e6b2d0a9d62f044a9a825100303e82b08b401a70c9cb78d44e9df5c6ef1f109056aa244a02e4494851fba3211e3191c9302a55de65f9387d68665f65e971cec644dff4d12fd0ddf3be0c43b805a06b8fac9efe4a44b0d3a8709444bb83c7bc16913662f8e7801d16e36fedf7951a5f445d1fd919e01e69d41d44ffff8ee1115e263c9992dd7381cbac249fad8e7ce3ff59ec18ef859cc8302664940136edfcf2787ab577041d0eba804744991d9bf934aaee6b3eabaa559dd50829d97e637a20ab6316bfe3901dfc4157718b4f3a68a4729e9f6e9b0a2100d3099165ff14f4d0a3f83696edd56ee4216b253e83fa4757584ad2ad8a8f83a35176868b0735b2c0539bfb75041c66bc5eddbc16ff61746a390aed08a78169c0ff565bbec707bdbb2965e1a0a978fb76267407c95a80ece720085b5aec323b385b66cdcdab1129aae0f2869d45d93733374ee066967f8f7675341d3e5f8b2d6e3d3191f3731b1178e2c38f62b9ca6398eb751ecc66e64df0f5a73bbd76ff7ffe2542d174d47f7b5e2c8b1a02098adb0bd5f9e65b30579ab1a330d33732c1dc15382c123b9424408905e579a57e83339d3a88daba2bf5b9dd659650121e194f43f0c68f73055d60683074ad5af03ef0ff334fb659b2c88e7d5c45bd41ca5765b49d3d23904a420cc73667b6d5e2e7832819f709db5a421e4b64a2b6f3f8be98029f9c267bfd12581c480e570091ef824e711bd822c15d2492fbbddbce94080a681e9c889428b7dca2077dd96b65a9fcec0cfef5336533697a9344bd63de3ae0435e77a66861939ca9c95454830d5c04e50420362d40d63e6f9903046cb32fd373e8043fee218f9e715a4e170c71ab3a23e5b7091e387dd51b4b560b93e2844079687d56f767c1bb9aed233e68b17c982037461e0b49cac997a23c0aff489f6fee5cfb390fc7b997a38f3913a6039f2bd25cf6754d2357b9dfb1d9b46067f675f1c55919e570f4c2d1a840c59f9e6008b59466bbb6934289711ff598fef932a6e9a9c168d43d6661443691603363bd1c6cc61aabb208e7dac01a269ec120794eea980ece05f335d3bb6520819933c0c1789be498a0cbad9c9937bc1d41542930692da859316db1c024595993e4158fd089a93ef14a00e8e492fd27137c87d3d1617de2f337ad8602fd542239d15a03edcc6ce69bf6d54ef4870d220e2af0c63a4ad9b0cfd557415b919f4c0e89b8f8acdf87228b8b706ccc4fbed62f77b0addf58cb42e76ab88951ba35273ae93a775955626877cf7ab698d9c1d066a38fae68e78f17c7be9f6cad1d2451daf015ab934f270c130a7292b53ef93ec452710152c8aa903fd3143c4454e2848bf2de23b6e84892dc716f04a8bad0984dbadbc67a0868ce5fd65014ce8d54535624ec461e920a6fbca3b686e276ac17168ef7cfec543fd088c58f3128a8424860f272e0d24d82bc070fd9cef87dfd9849adab0bbc81b973f0f2eba1ff79d2766ebb5d33757e215dda865ecb7be644424acd0adf9061c2c97291faa629624ecf6d45c192b315bc0d1814042441cb9f0d8f32e94c317aa19f279345773b1459d71eff5324a1a9532cd9214703425844eac6236032f40dd99f2cea73ee3ba8a2c2c59b0afa5e1fd4c74b72d7f7d621e611b04e3edcf6263f1fa7700662a57f17ab11752c368f909ffa39b79653b53b1bf8fe194cb630710459277c07ca885904055a65c1ae665d960a8f23745bbecbc1ace2febb5e006095b596babdb6ac1706a3faef3eaf254015c82ee71948b45b3d92e5da4152be2bfa630aa3648bdc454b3e515dbdbde00e13011420377a41b481b3cd3fbcb4ca7133ff39505f9e2c6c46429fa82e392ddc811cca454b8777d72036242fbb37008996343dd87658aa6388da03e2fc2431048f8b636cd7b5fca39e795f6745dff2ba3e09796c3301a1bc5725c609d2f778b8df3e84f784369ffba657824889e8b3d3080bae374af57506326072810cbed60b3245e961a24145d24e8ee8166212e2493e6bc5496973d5fb03f412d96c43ae5544e022169e2d660f1bf3115f14ffa18807a218637ee9d563f4ab8c5ee220c9db7ddd8921db1d3b7f7a24e0e64fd1d2772f9dcad3fbee3879ce33c70c215dceb3f80c79c227f342549b1a99f5d380dc62513033b1f2b3b8ca83cf6fd6fd8cfde3afae290676b1fd1c74d4c1513589eaeefeb8ab067e64c733f0b5511028f7d064c790652a9f2e51d964fbdbc5462c699e6091ce5851eecb16fec448be81bed1e29289b6f864eb4fec10550f81c9884be927f45e7b4cdfef9c879d66e71ed35e589eabef0ee47caa8b2f2619116aff9569999c0f8fb6f8fe3456630e01ac1655b832c05327a4661e2034db350aa1674df97b92553da94848c9926025b913ba92766c4c7495b68a2e7f6e8c0f798232a35d222cda86b006e5ac77a965d834d1ae1e35727fe6da64c8f0ba8650ce2b4fbb30195dad469455a62ff129f9aeb9cb0afe3eedd41b81b3e8bb87c489d442a28783404147899111ed682f3e1ab93b144a692f8207d0ba2374843737d6b2bf5c922d87b26cd4eb73353e5b1711d40ad6d2487ef7338c22c2b5a2dda86644325649f0e0a9e63853f9aae7d5546c564bc97e6dcb5405aa621e4fc8f0082b8170e1cb0706c307cc9f90a16447638fe384e42fddbb3a27861dcf1c3f98659f25ad13ff0793368fe14f761ef508e0e122ee7803215d96d8c332ad64fd32cbe44301862898249ec8f86938367e213e9eb72c28341b5142b404d0b4a07d2d53c224f3910253c2ba0ac87fc49d67bd24d3206934f1ace46f37820646095ebeb83e613ff1573ef34b5f8b742cfe7264f22955ac470590ede8bb0b2e205ebd292deb9941bc0805bbfe09f7d1d2fa4aefe934724d5da09d279dd0c048fe5c4396d2cd2e5aea238064d88f99ee16ef9de7346485d348a4bd5b9f8bc3bcc64d36534720d1a73dffaafcb8c9aac473e2ce07660929ef69ae8f1de859f57c3255192fa77007e3e50dcabe949495939b2cd793110d2925f2bf8a74f67423e9e7a63ec4d22f19bd4a84bc21276abf7a8b1ab1e344a004b8a0e3489d204ff04ecf0a4ca8242c7fc9f91c7cb37fbc9b7ff16920efc4f2a986fb497367b1d9e6b6bfea9f8dea7a1e20062bf3b883c00783a81954c6d36f0e4cd0626443353fff6ccd4a085a84a152b52a98263f11d828f45e0753c4ff890e683e561655ea42ea6e926d1a7aa449362483b2a20fe13057eccf9d40722b4eb8eb6bd3c03ff5d659c77ec1d83299224d53aeeea1ba2e838c6f00fb61b5f22d18c968895ce39ed5fdc0e5b9b970f6d4c9db2b24ada442ba8f71d61c38c064bc637b2c4756ea6cfbde583a1cebb7db87b3fa5aba883d50c17382cf5a6bd363edb4bf54b95815837156d2d6358d11dceba4a8aa4c740b277d563a585e225c13d1672a3b48806aae8413c36d90c656643a78ba44f52bb237d1ea719ba7e49f7417d27380922f98dff677841a4b453afd63ea1b48fe92ebce1af3901f66f9899fe6081e54743b0ae61476636c99a1322dccb4aaea89afa187ceaef10540d7d7c218201b9b233dea4dd0288994e034c9a4ce77d5d59171934fc6de97570d448bdf3d14b6bcf6a5e8f2814f2cfb86f812b409be8656e1cf9585c0448b943515c1570a7ba9805a4bc3301c47ae60143b62232e090492cf45d73214fcd2370086fc73bce1e5ba87f9aecf357bb3e7ed3ef74fdf609ea64f3923d1d954c5d7198faad69e19ad8993e218975e5d85375d5fd25028cedcead1bd9953ba10cf5b35549d2a6e2128d68558d778453fecb9fa564d02318d59351ef77b01325f468b2470809f99eb43f5f5eed7db8961bdad1b796d634894e7a9aeb941676b6760dfd5f49c50417cc39f227cf39ce6b9b75656cf977d56ec5edec7e2a6bb67fd9bc1bbaf7cf8cf6369e6f363a44bf69d85fe3838651e03702b348dd0ae188e49b2d039fcea66f3f9b08b51917d4ac731f2c5f28ae2227f996595e77a57e2e28442219aa2f59f7ea51edca321756f72d8b0099427cffb85571047228b2192a51cb080c750d1e4ef092d5a0c67fc33b45a7a0405de6dcd80a5d59ae77639cf90a76b11a0c31821333a59ca4ed3abc3f8550da7ac7a2a2802c8ca9a286e9ef0d78ebe0f48aea3373ed9e6c5fe97aff9d9e47b33fff9a7459958f078420454447f474edef1f1149d54f47479fda182a0064086514e780c5ba9e260d2f4bbacb8b96394f099e8e816b0ad817bf1b0cffce06073495fcdfdf79a16969cb3c81c81cd8d101305768bded286f9dcd3b00854eb40a56f95868f3f83e76e10382574bf2a49a83539ee8f70932bc0efdc3eb349fb4534f170d172a79d919bf0c32273a198a8580de854c0eb752a94d7799f26f73dba873f683e8dda109c8f0f33320b72c98fa2ff8f3b1b65055f1e0faf754b9552a1f2ada7bde68d350a49ca1d074bdac9cae652de1499433c6a2fdb6ddf20787db4b81bd218294d7a6448c7b5973f41d6391cbb7cd58ff2e0902462f551933fefbf87c10cc46f0e36cc925d8b3a08369ff2d467dc4535810820ac9f959598eafa9615726b8ca36141c990878a4ceea6dc3302d6de6afee3239f41d85805b7fdc60491d683e289eaac28f499954972be183ba8e7ab394ef6db12d1fd076dcb336ba3abb2ff9b915271c17f973fb8918aadd99d35eef7bf579d9a560099fa7eac878909ec6c6682fb9c48464f3d8d9d5b6a0b1e4a855f7715d5ec803dc1d40ecbc497216ec214deb0b30e058cf4d93a7ed7dee37087227ef768acf40da17f7174f477fec4ee7f835e27d88e163cc5d8a55a30ad2bd5b34003820445eb038ac10b2192b6abcd86d50aaf8d4a40ede230d6dabb8cb93bc0cdc10fb0a08de30aea833df0b83e920a18aaad2e99e3f605e8f3195a7f8b4e6fb377cea35b4d076b2e38ccbcbd0c982d69ac49ad863da2fa334e2d26117dde3343ab0835d709216b9bdd05bb529a5d347a9053af7b7aa9f684d7c2d7f179e353885cb8a44b10e103b9b01e511ad0f12d7ce45eb771648dc7ec8b0beed8f0cdb2be7d15c777a27c7b9e94157fa382c92d0a52db44a94493f8b9f316a9cb6b84723c460fe15ec0a8086140fe6ba07aaefad882d75d691adce4efa4c465fcb37fdfc8a47e99ff40173f86f3b66be52d65ce6ab37edc21c7695fe7801835cffa1809f24bdc6ab98119bbb92771ad5e54245e05517e18f46e7fdb2134361066bb24baea433a318a0c99b7b8568f775081039c375d41e2c4f8ee1aada741ca7d7f61105a618ee7c7fe445b33841916a7f1e44c913fdb5602df92b84443a0a7cc0f212f68afa1831fd9f640eaf70442cabbb2bff8a921d61d596b65b0931c4501a4797c1312b7a0944a65ef51bf3abd210107858be8b8526f2d78a67d5d17d27a8aa9253a8834e1dc91d03a0eadecc41f733e0537af6b25f631ad45246bb00d9842540e9abaa99883749216c857b7ea46126c16874ec89fbbf3be4e9d5b12822ad6a55c25d1396852e5f7026697abd603601dcdcebaadc1ce94d9a97a2b77ca283f4c2e04816a524c8336466bce0679f299d0792a0726e958b6de0a6e7507c8e8927f152f89e0c087b4b590b37c6c51da6e3000181e7dfb1a5e01aea3cc7b6545ea5543b77342f90f0771b90f41c8613546e183ce01fcad9e22c5d583fd5cdf166c80f393c1f44cf73e0015c410f49d49fe326f3526879f82cc3b355b5c660e2d75a562ba8d0621a701f3a5f5819d51cf92288b48f279bd5a4d86fc9ecd2421ef0583ca08917b73b4c7d2b23cb31de21500cc164d43a7fe3d54d6a55c74d53b06408eed29d0215455bab5ae084bac63c9cd29772c0952ee4ea66c36d18a9e99ca49f0b53164df6c73cdad4ee5c0c07edcf604696d7580952c33d37c1a3cea98306497b4e271417324877049be5bbe91f777f5f4100e91d1dd316aa7ec6d4757a027a15b64ea276609e2766d7eafb40cef32ee32cb2c517e3feadbef9a4f4c3c589fefc4b56618a897557ea13813a77a7eb0b076b834024de757824808b5dbad6ac2bd7e573c30d698dc94efaed90c96a62e207e7cc9deef19f153dbd29480f574400040f249c584bafc92045da957aa0beb38776ebd367493278d03d5b6cb5bb8459ca64cf4925ef3b6c2cb0e00d98a59da920db987eb20e1416a6c1bba660ddcc8a36906130601513afcdfff4f541f08a10f949a5020b5fc89983f393baf72eae4aab82f16a191a7187e9838c5480d6adc5a020c51f1f16ce63822ee620dd5f830709a7affd6995d4e53e5a9484f972dc410f6ee6e274f70115b0a03bba851c54fe97d6c412818b49b7a1bca44b8bcebbae8fb9f934597ff575675150fa94cea9714ef5d65979a7123e82b4210a6b95909ecdba21a7577d0c3fd7f5afe0e4ceb8f54720e3a49714495c1ce844bc7fb9b8d2f946a2f9e8f5c13552acc5ce9441d3d94f64988484c4cb7a6701eadb88ad0704456d441a6db96d4865fbd9013828040f88ca651316eb8d03c037cc2ab90a7e71029516360694a5c00c1c91bf85cf3d9cbe1193022dc11f580c23a9aa229d58264c87fa217fc3e19a2d03e1d44a913fc07ade9bd6a232596442c6cdd63edd81023d2897dcb763e0e0129de8e868f5214ce673fed2672c648b7c68e21d3c0f826ff90afc6c5e7c5021371abb175a1d7d044972c4d1bc6a9a35feaddd48a002564526b6240717f169d9f85fa4a95d31f475759eb5150636d2846b6b313181f69aa346183bb703aef25cc6a5e317d461d59cb4f2aabbfff312126602b55b872ad8e002418b31d96b3d4060094b23ba838afb716e994110b6726704fc8e3ae3f86b08155e1f38ce6b5194424eae3c6d69cbe8a743a3173f43de429ee3a353fd8ddcbe75fb1035344fd2727ed8c12f1536406d6cea9d6284070badf7f4a746c278d53c19bc269b413a264c147af85cbfe9b72b45ba89be10428ae70e9052632ff2dde8dcdc3a43d161faa733cab1c9b616b0790afa44a15dd4b821c8d5625913466ae82b031fbea43a8fc0626561dbf08d943617ab9ae1b9f4acecf68e64a0c349ea163fa02017e65a64f30645a379541e0c59c87b3c211ca129a327d36bec16104783ad78f32a8228fc2ba1158ddcdcd6f0b6b5e7b300c4792c14ae46340547106f8272cb5fd80ed2e9d24145ffc407da4a6935d4ce464b152e0e83bfb2cb84fd779ed876a9fa82bb7e0ec687fcc08b404a452a3400d3070126198f4528d6c7803774633a249cb24f47704dce9d2d1615c7bec2f1462afd2598a90713605d0c732da9eb14533f4ae51d665e7af3ad60bee0617e7f31a6332b74340d40866cd26bfc291bfa4e8db5c81ebce9ebeb56a49c5e318f42fe812fafd949a41c7611526f63ffe38308151ba9c1b2e795992733b5d5aa27a8f43694b09e4acb4b9295c826f42b8a1e63086e32856f020f079a96bfc46415aafa20538b52dd123835becb339a353947ac3ff5eaf97cfd769e7274df6677431135ec70f0a161144c409691ae6ab487de766f1d6ea0a4958e442a8c772f712819524d2037daee2069079cd1688f7a52132eec13281ece60cd48adbbb9909271b71536e9964860182f37cab7939f1a5d630db0725892ee0f61f96c0f4e7ea9ee4ebbbae533cec9aaaf564ff74d5bfeadeb3cb09991ca2cb7445794e84c33aae85fe5a228d750bb2f3e4edee29cc5bf859cfd404e97ad71138fd1259202353e50b50ca0ec550f2ac4df223092f93d3b6341aa7b381965f4430414df4b0d4c46ff7604706d1302d1553fa73f47d2aef4356e94a27d2f95f1ee1b198a0649c4b8a027d8c260229071b7f2df2318367b230223842621cefd93dafbc6a948cc0ae1b6a777cb70b9908be0654719b641af56b0064d983215f142975f5ec11b4f6b096eecd19bfea6f23fb5ef5f06c7c92cc7d7bffe74bbc3eabfd54a9b25df701cd8203ea36412cd7e100d9a0166a022eeb91f38b6bb788f7c965a7b968e8082706a3a1c39be812f07045c60b4025b67a668ccb1ca190a552ac87a8beed6f2278ddabf0e06a4a118907cfe03961cce4b1a7ebcc572c810631891f00d5eff226192a3a51a4cb9b4fa023101a1b61470a2d440097c82b0480ec1d88814274795c4c244c8f3c7e60f5b4f2461e9c5e67ac06621191e62be67d0aa928caf40d8b0f3fe15688d012026f074f4a7f7f12e2ce1575076bd9e0a8003fdc2712de5e86d8c5b0f0bf3ee907ca8495ec5924cb638856347b6af478996aafd299a8f158b6dda82f4281d1215e355a0a72b1584131e2cdab3f1cc069cef797e794d5c5266a66fdeddd964691e2c8a94f4ebd390944d2e07fcf7ce7d94b185babbc41509531c1136f9acab004344c67081f6248f631e89487439a21ed801d619f36e2f8dbedaf29b253e30931f6c68d0bdd2bddc001fcf84b7589432ba7b18bf3a811823bb19c9722e9719db394c1c5482bab30ccf08e0a983d3d4d75606dbcd692f2545212e2f9ca817de98736d144fa415747d29700a8e7a6e49f04093fe891f8df05a9be729f8d496bda47239b50dfd51560498c5d519346b72c819709f68afe41ab9670920e49387ab6e53afe5f7877460f3e2bb8a4fd751517174b8b54a6e3c0fdbf0f068c5a348dc5e5a57fe9b83b8e9b7ab3842c3bedbe39c91e784e6c19629fd6386c47a5a030e20d7834a0a81df739146aeaee3a18afb5a2540fd347ca774eb186bccf7ca2995162f462865f30ca1e59862c105cbf5569863f6b4493260b37be1651c31b70a5c4da56e79201cd6c6bff4201271d6fecf33ec927b9be82e8fe5af7aaeb7f867cdec8905a83936350a67e93efd7e1e3118c2c2e9a59549109a6dc2d64cef4521e3911c3a05dcd53dd0e87c5562677ef804f4caa6c32d203234ee49aa8b615ba397edde50fd493939756cf00ccaf68d6a4c1b9b45fa8d5161b73431f900dd61680fd951aa8a07e46a1d08a5f5a206b337362886d315d2baf637e5a39edbc1d28ff2dc9ccbdc068935dd7e99ffd7492df8daf333876f138632305e090a285db796ef83cc27814a343ddbdb044b33057808bcba184ce15ee97a5b5c8e5ac6e4d0c2b90f5c86e2281a69f5d6bf42569faeb253115cc8e3e01131354cd7da52c8052b5db1ae1f0e2e2bfe655210201fcaa00d9b87d0bc7f5af4b934daf6610bed673f377c4abae01049005fde93bf4aef3b98a166a6b1d2adad0aab264e83a3b85dbd4317b44b44eada54243d5394a2c9e8fc6c2717349180fb56b71d09b4dbde272eff1321d4c2ed97ee92d253ef7a43339d3f0943163033cc2df5e71fa95d4f811e3aba76ab34a58a501e566d2a078861a7d64eb8e77a0fee1cf525d93b1ac0724386c26c6a6b0c2e982ee6fe1b835fd1ef8b3dc8117e483ed89579a90b260ee10d5aa3967cd4dc8082640871cd69e4376815c834782c60dcc69bd8e429c332fa9fc0452928731a780baf698b4bff2179fa94d207aa747ad41a9706dc32a1e1a47c31b1bc062795d9926827a0d28f4ca12407fa0a6d3fde862ac5aecdaa2defb6790d4d235f75bbb3002037da9cdb34a6728c19b6ee0f02f3d5745bee5bca01907ea8aa809feb03d497f3b426e97bd51e0808337810d40849ca8a835f577e34bd3bdadfdbd3f1caec2d46baa4ef03162948f692064f45c24690e6bcbbc1b4b09c9252620a401c18fea4b7ac0e33f731e9466cf1e2a0ae441e6e24efbdb12cd53920728efa6684a34526c84104d29785e1e577a68969683c4142e0c5e76d0309f8bd7c130533009e6507e3d9b82f92c7f2490aed470f7ad9d888b0bbdb8263aaffebbeb21aa2b4bba39181b123f755528f53a4e2774f04aa776f15af9f6829e0d9dc29c77d67bf346b39761919cbc26c7e30cbbe3789ed8e1a30a7a725e3e4871a14ed6e4efbf59fbf1341aab8e0270ed64911e855d75aef158382104213bfa7f2445ad9c598beda05b90c1a2341bd601d24ffb4faa211ed6c3c4b8966bad0dd5433e4028ca667eee867bce34c90b0fd6e3b451aef8ffd358bb68101966ef5fdeaa12d79afaaa23a7185dace91a14f47524b4b9a07234b1feceaa38582e7fc825e7c61cbac9cc9361c9a862d523e8891f861a710d8a8bafc3e4afb0fb358d454ed4df084ea61e0142994c4296e6461155a4850fbee8a4465a00c646146083b72548b7ad9601061f7675ab16e3771028063488185444a04968f2785b8b5f21d2ac904c022aa64c5daefe09e4dd00fad80deca511171f26247d0c274f6487089daca3b5e3da732730b4cac307a2a1cd7a918f20a9d45c37ae3df9835b2fdc0abc60128be27cf98e590c60e968420abb9fa62d195c43d6f18bf9aeb4478a561e2d0fb1710a0c28ee8d1c61f29c40517dad7534abb665b427763e74df576dc2797593a514d385d06303436aee2c5128d5509dcb3485c6916c20c282843bff51f32233924f2f16d561d96470c0a1dee845637bfe3887e6fb51231d9602fb52d0a9513fc7619188bc7ec7bc0d1076d96c23dc9afa6225c5ca7f354be9babd1303458c038dcf70721e0610e49b026cb159549e1748546204411b1b425cafb9f6c6c19d14d8829cec930a4246701c827da07f41294aec3f84c5db1c40a2035cc1b6b39bcc3e0992670a4c9e543649c7d58159cb3413972dc14106811fff97b25d68602ec49bc6a72e1ceb7334126b1102362036302bc15082a235d905e467be6d905da84ae89ff59518ed1f1525ed213a4885e01b7274e2f8a963164b898799d89379c30a7059ff4ee7d4fbc683770b64d30387eff6d62efebe2f7704f7530972b9d83eb3d82349bbf3622cd04e52545eb352457987fa2c1e6e4eab313e5663fab7a2c13dcf2c48f19a87c87838cd0149a677151afe8ab827e923c7a6bc26d0f4ddb770bb28f31d77e70943fdd91bd3836c47bb15e593a928941e6ea9f826c311adb205a82304596413877be6dc84c44f34ffbfcde8f547c97b35eeed76e5e2ccb3906883d0d9eedfe38a2a0881bcdef45feb65bd945787aac2b643fa458a075ba13c1e4cc3a5befc9e23e73e51bfe25135e6b1fa20547cac9d2cd0d8d0aab5e4ea786daff16e061b744ce73dc198b8afe1f72b1c6996c48ddf2b9122965220838fc81c1a2983d8ca45c81527f254f860b427d7e09b3360c9711a93eef7bf8e7e4848df467515add0c2d07d1185b535fa6169f1559ae89508486e7dcff2793cc1d11c7382a4e3cfcf07083adf5188ae0c462c6c8d38053bc14af9e680a8f3e1d9f68d72a6bd5090bb7b3434721548dc041c59646edce45aa144358a9aa0600229695fb8995352f951910c0c13d99b82ca5b8fe562a05f187b3b1651d410b983ef0087d88347d9ed00d8e3766d12eab0b456e1504eb9ad85fc59298ba39045bede0af008498e1c60ed6622014cc94e057a4e67b67e4dd59f455b91a0c8a86a3a5e4b8f6e137699a41c4c67353be7d72e68f4316cca7f5759b47ae62f77606e3bdc1dcc5b74990f3bf48a41cbb959a27f60bc04374255cc458be0ef90c211992880e4dabd5d8ef5ce3acd8b304adccb1dc71d948e2278daf67e2113e0a01a79938848bc35e3ec8ab5b13adc29a2d30482e964b416f6dd0c83170f2038e4aa533954c46ec5008e845b1ff143a54a1a1546b349ca6a9d0a131ae74626ec05a691b4d509bed1f0f22041a37d4d43dbc44a55efaaecc4fedb4bd475cb64ef594f0131c984cf89319de90687").unwrap();
        assert_eq!(
            hints, expected,
            "hint bytes must match the pre-computed test vector"
        );
    }
}
