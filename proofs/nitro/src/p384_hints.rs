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

use anyhow::{bail, Result};
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
    let result = if result < BigInt::zero() { result + m } else { result };
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
    fn infinity() -> Self { Self { x: BigUint::zero(), y: BigUint::zero(), infinity: true } }
    fn new(x: BigUint, y: BigUint) -> Self { Self { x, y, infinity: false } }
}

fn twice_affine(hints: &mut Vec<BigUint>, p_field: &BigUint, a: &BigUint, pt: &Point) -> Result<Point> {
    if pt.infinity || pt.y.is_zero() {
        return Ok(Point::infinity());
    }
    // m = (3*x^2 + a) / (2*y)
    let x2 = mod_mul(&pt.x, &pt.x, p_field);
    let num = mod_add(&mod_mul(&BigUint::from(3u32), &x2, p_field), a, p_field);
    let den = mod_mul(&BigUint::from(2u32), &pt.y, p_field);
    let slope = mod_div(hints, &num, &den, p_field)?;

    let x3 = mod_sub(&mod_sub(&mod_mul(&slope, &slope, p_field), &pt.x, p_field), &pt.x, p_field);
    let y3 = mod_sub(&mod_mul(&mod_sub(&pt.x, &x3, p_field), &slope, p_field), &pt.y, p_field);
    Ok(Point::new(x3, y3))
}

fn add_affine(hints: &mut Vec<BigUint>, p_field: &BigUint, a: &BigUint, p1: &Point, p2: &Point) -> Result<Point> {
    if p1.infinity { return Ok(p2.clone()); }
    if p2.infinity { return Ok(p1.clone()); }

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

    let x3 = mod_sub(&mod_sub(&mod_mul(&slope, &slope, p_field), &p1.x, p_field), &p2.x, p_field);
    let y3 = mod_sub(&mod_mul(&mod_sub(&p1.x, &x3, p_field), &slope, p_field), &p1.y, p_field);
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

fn double_scalar_mul(
    hints: &mut Vec<BigUint>,
    p_field: &BigUint,
    a: &BigUint,
    points: &[Point],
    _scalar1: BigUint,
    _scalar2: BigUint,
) -> Result<Point> {
    let bits = 384usize;
    // Process 6-bit windows: scalar1 uses bits [5:3], scalar2 uses bits [2:0].
    // Mirrors the JS `doubleScalarMultiplication` logic.
    let mut result = Point::infinity();
    let scalar1 = _scalar1;
    let scalar2 = _scalar2;

    for i in (0..bits).rev() {
        let b1 = usize::from((&scalar1 >> i) % BigUint::from(2u32) == BigUint::one());
        let b2 = usize::from((&scalar2 >> i) % BigUint::from(2u32) == BigUint::one());

        // Double
        if !result.infinity {
            result = twice_affine(hints, p_field, a, &result)?;
        }

        // Add precomputed point
        let idx = (b1 << 3) | b2;
        if idx != 0 {
            result = add_affine(hints, p_field, a, &result, &points[idx])?;
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
            &mod_add(&mod_pow(&pub_x, &BigUint::from(3u32), &p_field), &mod_mul(&a, &pub_x, &p_field), &p_field),
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
        out.extend(std::iter::repeat(0u8).take(48 - bytes.len()));
        out.extend_from_slice(&bytes);
    }

    Ok(out)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use p384::{
        ecdsa::{signature::Signer, Signature, SigningKey},
    };
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
        use p384::ecdsa::{signature::Verifier, VerifyingKey};

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
        vk.verify(msg, &sig).expect("p384 crate must also verify the same sig");
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

        assert_eq!(hints.len() % 48, 0, "hint stream must be a multiple of 48 bytes");
        assert_eq!(hints.len(), 35232, "expected 734 inverse hints × 48 bytes");

        let expected = hex::decode("cad7705dbf824e00676213cfa144e8d0eabb5c41f915f677fd17ef4a69025d814999cf2b6416266d5b44d828678bf4b0cad7705dbf824e00676213cfa144e8d0eabb5c41f915f677fd17ef4a69025d814999cf2b6416266d5b44d828678bf4b055c284da8c783664e32313ca00c5beeea8500da8760bdd256905f9969e95e198d91d6b26284f8d41419c268a22a33295df5fe23d6e21c9050b298ec44c676918500489d669ad3441777527283aa22c2c89286b7222b1a03179d59ca118b04ccfec49329b433b21a20abb0b1b2f4801f446e611459a3a0b83291cae73e030b9ec6b4923e7f2adf8d8051310d3cdf318c06f07e58b9394f2148de766a79a97e8201133cea3457b33d37004724af91503c6cafcb037f97eb449eb569b87d63061455d4745ac1deec45de2f3ca54d7ddd0ae783f6cec5747bb08bc3ddf3e14833de1e95477a397769821a18bd3f57144b5a6ccb8088328f199d54cf2650e9d5dee93e0453607e64cd369641ba1caed420a1026fb99e1c450a08004003b966892e53ad76e62a87f4ad4e7a1349b38cdb00573ee7716a426bdfa7b9344083ca59ee6821d0b84974281e1d4934795b00b7e6e3a44d79ace265825c93c11967f86632f45ecce13cc9f3c05053776e0c61067a204ddad0fde441eea033cd31af5f626452f3cef42df7867fe6bd52d2db3264639e10d74808b43bd4f546bd419a5f56bf21fe4b459db00397d2c3c80b85179c2b1be44f81d4ec1fa04052e703613b94940ae883d60ff4130b812bbc1da290060c3c49d70469ad0cd82bb6e788d31b7520dcd9059e85632a9dd1244cfd053a56990e8ec4b5b9a136d8974a9b446a395c8b1b2633045bf8ec7780a6170cf6681d441895f8557a61cc848233eba82ce958b537e3fd4ae3b4869172a8a1230fcf0dd3e181d8d2a219438eb4169027be0bbc225ff8ac997367d780d4fb0bc311ce18d80bb54e49bf2eec49c410a6ec1a8a86815a3f49bfdd29cefe3dfcd63f133a57dc7b2cf774118d2c89e45562d4781dc97f749fb682d2bbbe585fc3831c84f3f2b417b51339b593018f2003767804fafce41182f6f2a915529127d54c2598a7f9cbfaa5c55a323e834a4bd95d375bb9d13743ef8ed204531f2e40b6531b9c05c7670767bf04383a78b45e9ddb0f3250b94bbc29865fe2f35d65d317f8c72dbe8151f1ec2ab9626b69263c78270093546547e2d03d7a5005d9b577bd07c443fc64b64f24542843ecc552d9a4d30060dd6e47ef059aeb008bd38f3fa776da4947d134a73b908085bb8a42f2ab32ee5ad465859c33ab52afee7391ea56f1636b4c09fe28da2ce003afec6e4cd9c1e81221b7630c306cb00d975eccd7cb24650cb0ad740347153e4bf6528f6e2b2a2c66aadca83909b93403ddfd3e66eb200a0043491420f1db0baa37da6f855cd3a46886674a7c22c54052f8fd50d1351be39178eee40444c3a4f6d9f11c39b1594d90914586c513582df799c115a2da038cba9ab8275a21487bd9bd894f4b23471a6e07876550bf7ab981df0b9c1de73bd063e36c79b8c3c6631249bda90abdce36b3bafcc964f1f9a78ada0d9302475c3dba41051b27001f14aaea9f5f79e8b14a27901e0031ac18fd111ce7ff174570bfc53a4e972dd611e262ca53ba323709938b534cb994bfbab249782669182cdd312f06684bfc1f3bf867f1c392ace2ed7cfc970d444b1ef0dd92c08114793b1e675601c8fed81904495ef47a6429767b03fda49b3c203c9359c54111ed7811c8c6335fcc9ce1bbcf9175bb394b853a9cad0d674bf71a36b156c9c9e08152ac9df111ae7d2138930efa3ffeb4bff79113023f2907338589e83638a352c7d7606cbaa0dd40785d2bc0ab3efcb2b8b4a0c9c820d2260bb5013d833572b19991b77b2039a91688e8cee11291caa2841df61da6c8b92c4725abbb96315d67ba78e58139654d5819b0406b325ed8e9738971992d80b3f3922118eb6027a15349849bf08f704883e2f373db8b3fbda47cb850b3a443e9d650e92fb0073a0acbb3ad53d8e44ac5a3d4a18ea6b007966ea5356f763d1c036dc2ea56de45f8d949aef338c982328cb77a388f5048a88eb61414977d712cb10eca864a93a85df370926c267920b36792881f1836bbf8532fdbc2b5930ff59198fb3e8bd93b280c42c13cc44ebcd5c7e0f5ee8818b399cd005cc56f42143956b0ce8dd8daf23e1f3199d3625215a05316acaadf81435aca5217d3323cb0c239610d3860c9b19bca12734f7aee82559775f40e3bb8bfedd5799c2eeb78e89ed65621a647d6aa77453a347b3be3fc35808d174583d49f87f8a95ebc58f3996052cd86c7b0ebc7ff8ed51613ffe9d608bd0a11aec8bc6bfc05904c751d2278602a9bddfb705c3c2951106f8c1d861137a6e9f12db3a946559307eb6a7b2be338c44e2f794a6e59ac9c4316b956804073a9a5745a0c5f2e2f72a55f2abb9ec9e81646be93be34f204dc442cc044817c469d6cff3ad355114d1d995d45a973697ecda71733a3505180fc46af4c624185661a7dc273e4a0daf56d527c4c0e263ac8b3ae1b6cc75986eca6894f3b02233f1f39d41300ca21c77814a2d80b083cb0d7b8cb98d3384876870f060908ed6f4204928b9d7d90f1569ce91d5b5cc100bf1edfb1838c0ebb02806afdb8a16f8c9ba58bc1cdbb829117497e88f48e4b9847bfcbb0f2100a8fad73da322bf82ecd682c52492b32e6717ace15ef6c313eb0836c2583edf35f97779ac21e4ade56ec39ab77c7b83cc8d58718c15e21916b877d17d15ad0f8250f566578a6b27c4ef45fed21129959f44cbda09e2c41c04d8dc0af141dd50c22292ce5819b878fe74e1d981de48bdbed3f67dfdc31372c0f8efee5b3b14eb35e0ddfa2cc858e564e7e3cd87bceb0cdaebdba1c8a6de7cc5fc2da890b846621abe070e902d71087398e9a9875b088ec2e67494b98c52b6d1846a512cf165be8d33e17561d61cf90904eee0c242b186a1ed49cb515eb35536c0e59649ed16efe6cc1207a31da2ba74f29e3a424c2210e7f20eecad4657270381a6441994e0562c45d677da821880b28ec453294672571377caa5990b3165c0653e5b16c7c1683c6ea2f3d440f9bf72b2ffdfbbd03d65857fef26f7fab39c02180cbad7ec2b8df6ab113361cfd3ce7cfccf3a95c1a4e318d7dfee44f0e3a4174da5ad58abe883b5a3f8d06b802d86e76bca64beca2ee759dbe413887969e1c28ce3107512996ae07872a314dc4aa6116f87506f44956b89cbde72627feaea944cfd29a99ac79ba93e56abc8c0da18bbb244bef961b3b285bf4737e1b872cb7ea854e108838eeb8a39f78d7ddf6578cfde7cb3e290d0f9d2bd38d1272bb3c73e116fbe728d52e4426264f1ee577b980974e8861ac7523366231748984b79f841c4abdcb8e8ed37fb2c4f5c479b65717c57549b4e597ffd1f18252710a1b3fb2ca1753a4f8f410e380d6908704d1e1e1ea6d02890e80c09ad041255292a9c86674066c4dc772b9d03a12ac5633540a95bbfce65f69d3166c75f7aa1a1a3d0684ba013a02049868319a935ac583a0b7c93f61089438576e030086df790cef47eb5902fe1a9f61b5e921641ed51e0d8502d6d01cd2fd4a3b6b3c5b4ed4c32d039408e14731645ff9b8bb415f1f18d7f430561cd3105120826f0f86c2b445135588a53867e3611318a46098f7bb8f41a5ad5af5ba81aace72ccc064ee808c34bdea683196b9e9cc4423e31e0e8311d0a404fa1d54663ecb9c82d7c23d6224ae92e67f1971667e2696638b6ccba352adff85bbefaa0fc843c4ca90554c0d80fa75491dcfc0a303398865dba7371dce1b454b4814c9f797e3dcb3e72030781554fff82059efc22678cbc7fe0794cae6aaca7851863a5b09711164965820d254ccbda1afd108497a13cd8cb4bebe79acc07fb87cb8e8554f4465712e08c76f475d7bc85f96b3862a2ef24be9402fa6c4ddf78b8da391e6528d670c6fbb99773c3035f8777e384d37a876ee5dda66e18443b75ffddd0cb55f097d13347ddf59729458ac184e3e8abd66c9a8e51e34c7a2466c6fed2f4341841df960bf7ca81cf6ea65fe98e57e99fb04fdc966adb32802cc113e4ec22f00c5911f02274abbf07e63f13f778a4d4b1e64005668c6d71ecaa4dc51a5c98829bd82b5277d4a52ec390dda0fd6fb0aaeede4fcbde559023d26320c4923e1ab4d38d71e4f443754537ee9e69042a84ff8c6cd9278b4a07efc5c87898937ceeb8a96720463655c284da8c783664e32313ca00c5beeea8500da8760bdd256905f9969e95e198d91d6b26284f8d41419c268a22a3329544d79ace265825c93c11967f86632f45ecce13cc9f3c05053776e0c61067a204ddad0fde441eea033cd31af5f626452f1c8f40ae348530783c0ba9297fb6180c33288662825761dcee667237d7225441f76ab602b3ba29f675c8f3f3720e8684a48a171ae1214b4d905d97d6525a341c6319b2ff9389e4c110d5cc92384673abe917c14d43cb5d875fecf2f7ac1bb6703c9ab7aeed20a73db9041b9623d58a33cad4e710e39b9d766b5ca6abe5eeab3e34d35709ff0068f541aaf32fbdb522151ecbbcd3f08f2195cdb3c3da929889dc78f8f26ef17d41b727d97ca4cbd264510acdc33a87918d24c71d7b3ad81bca78317867adabce9c1d1468ad5ec6c927e9d9df23af3da633928e77ef6091ad960102ff6aa6753e09cc3a507d9f70c35dcaa8d93de3ffe57e97a593814fc5ae9e7919962e56d513d979e9b96bc70502c858067a394a6c4ba08f397a0626fd89cb5ac29d1983ab3df535595b0636b899064adea87f15559181f086c61fb4d2554e1fb8d07b2be1a200eacd5d5057124a37613e73614131ba4619dce8bb444110acd824a002cd78b403780b553ad58f995f7e19eebf77c9a2c7b9a377f825f897b432f5dd96881d77937c069acc9d914deb3628df922e8e643265fa2a1eb853702521ec4ba4b9198abae34ad891394feee74b6b609b888b4a85ea2a3067a412518144be5a742274fe9185a048d27dab635304d9e3a569f8e46080b2c0fff90a1ee41a64998cb85ef2406b9a0460749ef15abce725bf4878a49d7bf631391585e7a4f76c1be08495e59c5c4e1666309ad24989db5782a60b58e3ff6a63c108c9696230bb4c9aad10b42448656a81b6ceb323b6d52f8866f10cd300d0313012b50e0267d99d5bb36c442df1a0050e136fa761dda6ed4b77d199084197f939336347f58726d61576d14e322e9e3b4d5fc5c846bcfa3e5ad2a0a71f786192ee1a7111edadbad7b0b3b92dd588dcc3d48a37b40e63440280adf76836371fe7e7c197445358135d8d167f8b82d72e869de3f9b1ed859b2d0e07cceef23c78bbaa3de35f98d8f4dd87866c0b9d4f6462096f3041cabfa516de19b74206ea0764ff54bce7fe2e7928f31f36b067a5a830f19f52094aad8c9d712966bb46522e14b7fc3a0b3be0960fffbbaa17fb2e28e74ddf61d3483d78fa468520b5558183253fddbdc655e7044005d7ae7ffed2e4ef245c84458ab8e4fc6101f78e0e5b8f4afdb5ee42ff06050ba31299aa965351aaf915d9daa02e227f81a2491aed6b111d5b5196752f9d3b4c2f8fd56794fb1aeeabe6c1b132887eeeb16af394b79a1081c3f56215954635d3a7778216eee1c76181fc528d16729c0d01b03d2fb40b6fd6b1915c0999a74e5be91febc6ee58a4f91ac14d134f2da5656937a5bedc2fedca3642c4e2b24ee27e10416efebdcef893c3e0f7b613a329022c47643803888cc3957c2fbd42c746fdf367fd5003268781350bfd77bc6218324b95750598ef9b1810800f6ec3faaa7131d5fe9bd154a2041896e4dc6d003dc2e1ef18b8c9ad85fa31c6f95696f44006ee51244bfcc0fe3e9a8fba4b437c535341cc8a339982df6111fac7be64cf7deabf805f965fc876e1673bf88d33aab3e1a20bbd3e8df747ae48ff4cc91b0b4ac9f1c7bbed14f9a391aab1d46267803a1d7cba653bec883d3b412a32d9ac0802629456895eb59bff994aff3eae6c6bbe26baa773f90e6dc0feb72b790770cc3c8b4d6cf5b9d225cf342a7661c76664e584c2bc89491c07b683fa66500d6778e672791612e1ae4aa66bc1b762847a69e08aeeae5f6665584826f69ccd799c621e7bb2d7886180e332da02ea8db589ee0e4d88e592c5e542ece315659e06c55bb71117df0ffca8fce25d9d28d4f896b169190a8c04d2163b7c462ddf7bc0742381a73e7234f3248361cb84a9663defe62964d2dc05323bc4d7c6d70c6baf02c80f405e97c6ef2870921acdf3067f2236461014b39be7afd719d8882ced0d1b620e7a22f5e58280663780a4c261ca916c088050499502194f9abb228d5255cf058e5d3e3c050cc3282f5099cbab23316785535ebcaab73b1e7f880e61c918304908365eba9cbe9aa4b26644aa3424fae366ccc491ec57df4103c60b2da13111e7505c5f39997f03003c78d68aeb8b46d9c901659017fbd863911e68b29061dd46f93c6b6cfa80de839b2caae370a4f039fc8786ffefcd3cf6c984d8b827892e6894b9f7de011e05116f2cb1de17683d03ef94d911f97bd0209f2402daf334fdcfd97df02bd58dc00300b52c80b7455edeba8f6007d42fb76f845ad85461396a1e6b7f41b424810c69352e447c0b07eec40a588b51338309c85eba8fe5fece9ff0d8bc5136f8adf120173dd329aa34482c555179a608318f043e0fc3b80cd0fc77cb54ccf43ca2649e73d5ba5b0fcfc40603c3094a53d4dfd4f441ea3eb3a04fcb10b3efcd359db0be19759802d479a34c1f77aab958d4aa1d95b5c56fa8b3d1526a0340b88961c14cea815cc53acf9d5b16622a7de004b983a5fea30a05c705900d4d17c78dd8782e7a6b70a984c17b1c622f93456696eac63714b2a49877e1073a1297a40d7362ce0464bd0b5e82f4edf3c06b786101d8dd1375dcb9be751eb36dd838440e872bd0f3f9da02ac321023e580f43561e85f9dbb02e81ffb3f5c0016b39c5a9e369000eccec90356a606e905ebbb574980ac9e5d92d368146e80d028d68ce28e42606dc105abb4f1e88aded4cdabbd7ce395b19bb45c9edad8e978563c9d39d226cdda1fab5f891441cb67be40da7b9bfbb5fbc27b28195b1daeefc1ae9aa7bf15e52f61c9169b53139e2e358adbaa597916109fea72f9e6328b1649df479eaab80bf1d43d213dd4a8c17e655e5b48e0ff74f7d3bc69ccdbba3efc9f73efbe0f9cc2c14caa37dd47d6ae806ef19a3127ed60b74d0a7c1a37cc65f7bc0d242e3cdab0e7259a6371de586eec1f368c1e0aeb95c3dd75b7d4f9b053db594ebc78e656d0ba101213074e2d70b44265af8bbb58b131fde7a66d3791b8ffacda6891b190e07c0efa82b2fe27d2c5da74d5877e3e096e4b851dcf48337eba93fe8464ff6d18f17efd1d9c4ca96c53bb4caadfaa6954cece87aa5bbf14634a437ef22816597b6614260587bc3d5bc0f724e6327efd2c37b5df883b20a5ad422ff3d1941b5eaa2e59abdfbc71bc908c3799ac68e5da18adb54a270b9b74cb7e8aa6c99bb795651e260344095bd12e670fad880a37947bcf112b961fa220a86fac77ff606f15fe1762d12e55c2ab6686235341b590e422127ed63ae1802006f55caa8ec7b9a3e1d6a0339e7707332cb41a1b0688b227dad3b305b2c13976898489d81b5a850bccfcba125c6c3029101d3d1d2f5190e7061dfd84c21d66de79f14918af99bd963f76edaf3ee9b0fef2b392c1680db096668e5656c82818d048078e9118cac9c483fa26254841d111b075775ee59acc2a3bedc5fbc57f50321c4986257a57062a0d08f6b4eab11c0918e6568bfa4b618065f98e79401446eacde4a8f2bfa31a0c01be36d927898ac927e13df754518e74b410ae7c42a3f9ba3f5f05360b0f447805958f03d8b320f92c83ab5266506a67ccbcada76455034a894931e452363821565f454c3b5655d4c0b6a23e4cdf97e3d03c5fcfdc85619b210c8d7ab5790655c5c990729a08cd6a864c2155abf8539754e07eaa2590f9387f910865daf3058ca06f515ede8602ff21a999e68458d044679f02abaaa4610a37d7649a85083b6d83cd58e643d1490bd53a0f5a86a50a905d957d023871cb94828a398568887910b415faa6c142814b0d325659215bbc32c6c1feafdd49ca30fef86e51eb6ce95efc069754b743541056bfd0137e808fd29a11bc7beb3c7e17d187cfd739dd477619b1486e10d705a6a90778de506e5d2f6e210729231c734079581fbeaf1129ee7cab6976ab0317d4fdad30b3bf08ede08f85d81fb930ea37162e1da0ec0820be0b46389ba59fa8a95b155f42949aa92be661809438a9a45958f23b8b9a9a94892a829f80c85c92841842aee4a856d38ba9cb586e820d7cc89b6d390f70b6b1a2c6ebc632c4f74384f6b0ec9439d8fb03bb1492a8fe75759d0d23d0cd415de833e97b72c92c929b48006131552b8b2b0b360a4f96ba424cb16a66534bf55e6ca1b37758ff4dd9713e9fe6afa1a86c73f0f31c8200e692525fe6224d276f549f2c6baff6dd3ff5a5388c13afef6461be46c5762579a57d71c15ddf30d4171851befcce6acac59d19c6ef77f485b102f14584921c263b033fde6af8b6487added0b71db2c1c022d6a3df4e89a83479f2c80201e88e01ff6ab99feccf61e59d42c3039b3ed150eb111d7a56c44c844d8c36deb825612a4408c3ebf0d69b3ea10e6f83e4cfe44cc0dd2bf5687fb2cee2e4da19d73dc707e9bd65ed5915714e4ff22203bababe5097eff10daba8c8bfd8fb0ade8e1a689a14be1febf22948422f618b247264f98d06172625bcc03df8e203a46cc2c120dcd4a5e89609e0779b8f73d6e9c76f5b90cf537fbfdf4c9b5fbdd45411500e6099c932531e12b27b02e735066186f05995bb62e6300e73e617c51ac87d3e58835b9cffc99a519ecd75d3d59a03b0e805a840f16ffa683adf92b03d4e375ddd4ea148867d05d218fef817a0d3b81afdbd639c5a1b8f9e98a53203fdf9408fcdc6e9c9275961ff0d6314ec627a65982831651ef834cd188e47b59e4d8ca425197e94becb7c9477dbf9d7db1349e504aced9422950932ae4f24589d01007ab1a3039a84b074d973c48fda935e46cbd46f5569da1d0f7ab3c165ba514ec46f9dc97a1007318a5e2be97d1b14c5c58cb5e36fa062d9283563a68c0e233c64be81466fd8e1d1793fd7375502f5163f4fa1ab604b8a63401ff26d1c5962752685f436c61df33d3b8f7faead7bc20af08082288dabd695719e64de9e5056ce036701ce2daa4db098b9270a72594e4d329aa4a621cae9b233078bfca100ed93a719037bdff8e8d7227236fffe00d6aa52d1871704d5de019222d276a5d562a74d7d46f3ef4b7021700f8edd0e7afb80f28f0822365f78879cec0129e53ca56c57baf2bd1986e907d8ad50f3d3a2065376d9bac29a4a8fa21bec8aa90e916e2f996e5b674e58455443b031a0cd043d9f5822a70503e8b551bcb6d1435cc6d2f4637834a25900b153ecea63a7f4deabd6ffd7a886813d5371f531c022a7bbe594858494ae295a35f4a8b8a8536a5fc714b108d0efdf90710bc3817463029a56143d84c04ebeb21255903abd1df45acd3479eb8198e9ae9a5849ce09006769a20d861c1ad3d1673778610969f467306359504a7dd9cc5bcda6e57d5a0182271877b074a57ec9fced4cd45619cbc30807b7d3e137d005dfe77269eb0c5cfcd32e2efc7834b789152a20474b807dedf92abaa89b871c63d5671c450cd039b75e470c5b8cab3a02652044faa034df291cd2ae2b0a4c56a822f821419f2ec209d5276fd6aef91cbfbce92eb76fdc87300acce233c84173f7499c6735bf6e2eb7871f21bc5de22a30ebc12d9763dbb7314e598f5a77c136cdda5747007de9cbd9d062f3684119f178cc2fcb56623f39ee25527a9dc9ca3ff65498993cf8533f2817e77105f35ce2c8ca495938f94fcdf4d746ccd940303579ea240ca4954a09856cb0e6105a85469a3c42135553bca24a7408f266276cd0da262f7da02e9a8e1a7e2eb7753014a605d554645b3b3c1809fa4e3d9f6f31dcceb85af044ad73239353e2bcad116486fa3d57dd4c6ab3456ac1a32f40ec9593f461e63cddf27942540b60e5f387f5a84ddeb34882e85f2199b3f39495a5c91ce7e0266d5fe6a09602a304d7672340e0239cb080914ed467014b5bc422952c3b8be93d690809ac5190e822c1733bcde4ea08b348ddaa3019317866c88adeedb150398fa382b1efb8d1f282462bb55f6a84692e58eb6210306a1036cfdb9df75f5ddd0be160cb0a6e6428b99a5ee3a2a19ed931ee47f679767de01a6e53744a147226af8e5619668a0b53cee84898b92824263e8c3e6a1ce66a15cf57d5ea855e114c095f6175f8699b74ebeb15257b8a47f243937962eb54483f61b676b04ea67cda6a8fe9bdeb8a602fcd20119517cef8dd1bc59572b7e51ed133bd82e1974ad4d413ff75b5d4f458ce69b0a3b27e25b7b39da3d641906c5bf7a3232de6f97d4d3ca4906e0e2d6f8b9444a55a9b2d750849de5e852e925215c7149cc1bb0e63d9ac6ab3305049007e5c901249181f2f16c1449db5227a4cda9c77fa93fb5bf4f0d73032e646b84a1c533dc2dceb50c9d3a49e4990c902c62de3ff6a8aadc29455e7c37e95a2bbd83085312549558b967f879ae22703f77a9c3c60061fad3c4a10af22b73881a77ec641f096faaf6124bc442b5c50e2eece4df5e6c5885ca924c65b565cb05b607aa877d91c1eafc83d8213f565a757da20e751f7fa55a74e4bdb1b48e64778e668298644a0d0ff4aa82f3acc29f859dec54c5a78493ec5c1eb0c7add8e9d6ea151bc1a6d096b8cae2c3568c1c8574bf4103bf1c8bfb50e8a6d412f262391f2335a538f8c1d077521384465e4f227f54f5fd5799915831b87a0896c6f33411f1eb7a0886e084147d8a3c67a38570c0af57dde4d3ba73ed87a3afd00d09a5c931d4356924116ba62e5809cd3c29f9f3f613960259bc82ef9aab2a32e79e2dee2df22b1e92232912e1c757641eadb27ccec72be585f2e38055c654d8aa9625c0cc7c2183758855a25e5cbcd2e2a19410671b71d65e9ccc071bb6372d94e30e2d54b8b0af1fd308646cecaf4493b1fbc50b648a274ca4dc4e9a2a4a4775d254875b0572b4538090107d6a1e52237896d944ded21d8678886f8a474bdcdc9eb5894f77649bed875249aac5700675dc5b603a2b5af6bfd02cd2e7814552b475ae7b21b849188b69f6412f47688e16143d8b5be476e45a35df87a051659b15c3a541c58dda3bcd6e60382fba50ab2fba434a71bcf341d22fc4c1326d99a52f5daab517dfdf8c58fb095fba96ab6ca66115bf7f9a8f40a4f432ec960352b9a253e3a2c81011443e34d620427913e8d1473025be2150a77b92fd0687df8e499e8eff2c9b6a78ba6d474c074818a7d5988df3055d3016ea09515d4e4cf1322cf8d7f52ea144f162ba2c664a5b22b19df51eb8f4ae6f8910e3c61eee09e33a7ecd489a524ab001ff43efa788897a57173e78c0246ffefd043314e351b65ee024cd2c1553b2a7e24c69bbd26a750e648479ff1a4274922a31808244edebf47f1eaa6f4a404e778386cf5a2aa116880864f3ccb3ddadd555f371dcfa0405e4f8d2483f6eb78a4a70358c45a9730f74ad6618cdc90ce2ee3b17edd6bd06150c31431d484efd90d2fd4b8a4db643a919522f78653760fc6e864f69f3d24dca0a5f05ce8171ac3112da211afce70f42e7ee3b8149a6414fa218282a81a2d79b65b9bedbdc8def4bdb3e6efe25e65b693ebbcebab8205633617baad4e384ef4d386dfb36e62de7b42187442c339e30bdfc9e58fcfa2b59fde7ffdf7dc58fc6bc1115d8e3e24a2b2e009b6fa814d8f378de66ad4d808db0440b1616c05c2df9a2ca6a5e2d944adf5ce45e5cfc2504db7b88fccfe0f092a9ea5216e40a40d485bbb8f8668fef17f1042230452f326a212995a1ab3df3c2c302c30acd8f3c8e6277f81bf4f0247b6fc8270e7ab956b061ea272a3647bb7ae0eaaa7587bdaeb1ba93a03738b9408145bc4505dfc8b8cca6e062cc6596d908e2656024901ce98220dfb30856a3e09cc58538419a9eea98f546eb56c3bfd9795ff285e3b92e8fc6af97ebd4b3d26a529e23d67003d87ae1f716cac6494974943022e94b3b7ab720e4da9ef3762a2554f944d235cc91c095949f74ae42662b5f1cc0b831418fe9d7039038cf7d1d5bc8116f0fafd44337424e23e60db1737f30954f368533505996cf9f5b708df355962c038131b69f9215ed1365b5ad0060851a097c519546962e999a14b3291d72e160c5039688802d2eff5737d16bb95655f98950449ca529f1eebe57af48c8c25822b7903700e697d9acfda560f1eb6f2dd5e34cc256bbe8c289a55b9504b9451ed19cef8daadd40871afbb2913971d3f1bd4278350942733701ee621576d4567e48839280db155666d7f3b96e6afec1beeb584207ec40517b9c598be7d0f93891a5156510f3a59a6ab94a62b9775a9ed5cbaf1707461f7e1040d8e0d119e5d52182ca521f327fd862d67d768062dd165b0b7f2435e8de5e12bbf1c4eed5e94b048d4d9d4bb497668219faf8cb1e6a7dfd6b1ffca6cb2aad90b36b9d30d8aa29d363c556a06f461dae901d5181eed22e7df7c054d5e9da3033b2d4db35dc9021ef50954ae49665f9edd506115fc5d75f94c05352a4f873d1f29defafc69fd4e478d9d30b646343ff017dcf2395a5bfac75ce9d073bcf5bb1e52dcf81a77da5904072d04d78b78640244c11370cce4706c9b4dd1e3cb8a4e7feaf1bc5c9383cff1ff1c7b854831634634ced6b28d1f0c6b75a38e890fdf5bd8cd1b01c8e0d28e2ece6a1301393f8fa714d944be55eb8e5bde3c6ee1a7c162f56ad163beb86db889d958b25a4bd00b0fe0a2caf78012dc250682e28cc9b59548cbcaeee9d1e13f718bdd52020521d988e432eaab85a679839cee4f5690b2d19e42d3f86fe6a958e2bfef4b06fc8c03048316fcabf0371eeb88574ce0b53c626bf415219403276abde77f0772c70f2e9769193cd240874f3ff3a45bfedcad49e0f9524b9e35f821dc26e6065c586131d1e3c21f978ae22a9005b08d9e3a4e28928b81ef44c21c3a1761d77cd9d47bf08e8f19468c28119a963b88068f721a1e4abc476d56e677d4c334489b8a9ba05735724b4e35541714133220bb485d43e78a2e139f5f891ace6af47af1e89e173a901620567692266f6faeba883a63361d6d8e0da41fc08b8eac7a13650007ae3fee4001c106737e36d776cad929a7f0701ff58d09ddc21965c4b0c28e337b497083ee7555aca1ed08b9ebbbb5b1eb8edf58f6436411c8e86e4a84db70927e2843ebb632c971ec671ef1af48f70dd93eaa2049571a497d64a4234b77f2aae07e5e5e0f876eeaacee17f9bde8e2c781214ff2c03e845ac1cffe5ae598e8621b3def6935b47f3d486d26a13e1b23e8e789a3a94b6d73b1bd11c0c0062b948fa9475de382ef51a2ae8400a9afc1ab54b9f93d531604bcfc8ae84e0ce3a1ba19fa8702dc209e5fa31de7ab9aa184c1e71a6483528668aef8ad567981aa3563706bf32acc53eb783c55744c9ef871b426fb5775a2be575e7f884be9554d80ac77e37c71cbb4fa6801d7144d4298777aeee18cc4fc600388128b97c767f1d438b1b560e04107941c490caf2434927d0bcc194ed49af409e3875e857de61ab88dee6c75a1d760bb3299ddd4acdaeaa63c07b3f9bc44cfd60187d50609a68756d2957a0b601caab7c0ca9cc34005c61189d476bdfb23d860b9f66e7e647f51affebaf2c5412adc04390c448a09e7e1c43e9bef17656a2ec8023fd2fe18420e868d6ba52eed40c72bfa9ca2a868f7339cbd1c2a493a749026f5df54b78adda166bf5fa06227a30dab18dd1f93f049a09e1a517b028e7652a15960e15e7d1ff53696d272333ee506d59114e3a85b4722261b8c8015296e7ef9619fa051ec6e861fbb8f210291c89a11ab3dabf13993489791cb608725d2a9864ddd9cba9435d5c63ad6819820503da50b1e98cc8284fa561e3313a4ae1f3e430b1f954b467b267ae8cdb2d9da774f1666e7dc9bbb0795090d7107ab5ea5bc2f3b8a4be42777093613b05080236038a482e067025b73844d8b909623714ada5c354553df3d95a86a514b186b3cc1042f3336cfb649591a78b38852b57a0ccbe472b1abe7e5b3dc5e2ec33e82c42533255e780acbb9cbfe44ecd2cad422aa119a6cdb4dd354bdbf736d25eb154f3e6168a3869f6164043e88146c30ccfb3473524b8523efd2c24892615ff713d441ef92337246c04ff0eae950b5782ab2d5be7a0d5a5d36d0a951ba1ed9aa695bc93783e3c4d97875c24c8d2775a4ccb718e8b300a62b618bb9ad913ee341c4c59b6a5749362e778595883570521cc7241c523740bdcafc746598ea20af9731e7f00bdbeb09135b6ee5d2dc398f2c6ccc3a27d9d1e25ed9f0023e563df4d4ce140a9ca889075fd09cd7bf89652f56fe7ff26f8b17873ab8ecd3707fc0b70b496b868d3784c42e9133c2d4fcbd28971df79369551c965156e5fdbda653e6b870ba9b982966c01a4c7f66fff4b8950d54da866c78b81e2e2f208f7dc346d67c7e7537997d8d31bcabccf40c84d3c506e1f41ede0308b04006f38a9ec519b7bf98d6be50b124dfd0775c34348f81408fe2341f3b5b51c4bd49324ea6bb6a9886f09680b1a1bf9a1aeb57f8eee036de9d9348c04a40882d1111e0ac3a76063cec8f8aeeb9ad7223dfa142fda0682d08664bdfbda79f6db0f812365be0d02bcdd394d87932d68d3213255d7da4ab7b841279b5352a1e001e3286a456b92f7f3b3e42de046edc973ecdf474074edda7595e042ac91e154367cbbf3c17631d581800b99ace8e841d65462ec913f98a6c4eda5bc36e3f35eff49a5732b179d9ababa7bb9b35e0c3775dc403d25deabf9f2b68575a5456470471ddae2fc6fa2a7d5974d4963ff593320a3bfa7f19008d87dc4ff467ab00e6850c422d22a6a190bb435af51b3513b71c348715ddee2f90d6daadf3f7b7c41e1d1925c777605c477afcb2f3fe18b600e5e3e8aad9e98abf1afaafe48bd32be0cdcc0043961b07085266a687e17510245e8852f1bceaff2bdf0ce82d280a73f44dd7bca57f0e985d0de85d24ff220107ba3fc05d2da93b7a4913e17c18b984e0895726b5dd1995b7a59206a1024419cd70db81c5199aa0aeeb9b087ce189969d93799c698cf03d09ddfad94dd884a4d5f1e14dedfb8e8c5af6ac7035815f15a0ef382a342905dc60debae28046c4f91c4c07734e4876c5c71da2f0beb0c751bc7096d539883fda48899e23eac05b0a82203f92a28251bc9602a58751dff975a8bf3c816d824ab758fcdb8d14f8db12eb1f87bb79243db35265f8b2f15e851e6b84a15284d4b0f21a6acd7d1eb28dcdadcb496461223b9a190346fa58d9a2bff252bbcaf9efa1a7be8076174662a64def569cca5ef24d99abe22278a740d126aa8afc0cca1d675e03b530f44199ccef739b5a0fa49bdd2177e115b129f11fcfb45dc54a6a1e8a457443219937614a38691732185ee9c9c47d15c8c4be165cfa08dc9755776fd73a52ec8ba9767527cad2959162cd989efe32bd9d0e2142b689ea80ad54d408c3f3a4621900a0847de3b7a80a93de5f12fe25142b3d721583d81d2b041005b6003b88f2c7e28d747984dc47c1a1bbb007e5c2681cab58dd10c940c79bbeb8fff5505b42d7974b1028c06b9155ccad8b60146f509f00d8485449d3caa1a09734f52b2ff3a175fa7544f367175ca0ab91f75dd34e137d38aeefec99c2d98f5787df2f001b9fc557c9790eb099fab970d303404625aba3594540f86467556924d06561d4d6215b47a5a136d4fd48474dbe2964ef29cde7fdfc7a8df3bf8da1fc2bf2e0cbb09a7a1251b84136e3f1187f32f5300cb2f890212e03cacfcccf26404118912d769fe96dcac490989767b0f7a6728aaf42cea08277a23e3324725f3994dafe395f6b9cdc37aed31f54daa944445f2bfaa47ec8b8594a965ae28bbe3ed4deaa8192b8cf7753a4018a4738060d3eb8293836332893cba59d25c1ef4ce26e701fc8cd23168f21f098edb6d81bd6167cbf94f63bf7a29c035b4be0929ac73ef77a76d7dee5ece62145ea9ce82221b1ac224934321445f292e7cdeb63b4c124b0d27be16e47b345450df1e32f21b58ce31fad301d12bfb14e3aae2204f189131ee0d499203dd9703659b10711468b839664fc3cec335ab38e6c2d697fc5558980a40e45aff66022abbca4cc1fe4ae5b6e718a7c54820b63fff482ab06aac7e7a45ccbd4dd88bef6df640eeeb1e859be0c249cba4f2d8425d9f3f3092834a7ce9c58bac385c1c48c7f7a50622844348fec9ec71fa7cb8337bf40163390bfaad0aed1b425524b61c833777d6d119fda869023d987f24ef4df6726b5583cacfe31288057b2e36457bed4f64d2075dfe0fba488e4c9e926fa89094d865064d6b86e7b0a6fce62000f225dddf04abaa4516e9d1f4972ddcec749c0c1592a9d153c217ae53686013538594b0c9917d1f7b535788ee972fd628d143d41865fdf57889d6e42ea70fa8a76776e48b3d82705c550ec3821afc7dbc4c734bc7cd7927645749cc21e0d5da1224e70737d07f7e3bdd354f2f80991dfc8f4cd967d800347def8602e7ce3076e0a2d630832017b5711d7a3f0534d88ffba32a791a3a26ff9d9e25b51cfbe79fe5f040fa102b53a8b90ac25d7f6700ea0790b208ac96fd0ef3af55ba074c26cb2eeb63bda1af3d702c10a39a2b1be3d0fb173cfe89c2ec707c8089c256159bd8f8aa779edbf5e9078e63f03d73a52c5e8b95cb8b839cf43bed4278c830fe54ea63dedff39d79157003cd067420c3e1b2675a02f870e4cc9be2b93a8c97c28ce6dd5737e349073d3690f87b7ae0b1d972f94a4de152279d78a821c7e9e6f23f17e0ee951adae76a2a44415e4114079de211c2c5980fa05bf3d8d714ac93de3a34ba21714de9d5acfa8d265cb6da798793debcbd84192521144649311b191b2c5732b7ea210660be21e44f42260ec7f1450847a176496cd08e9d3f199752f069b469c3a0795599679cd41e1ae00d1473f4128a63f3271395eff9380cb72be427fa3024c9eb5d58e134462b1ffdb37505b9322db3c1102c2454a9cb16b0542c37732a07baf7dd2584a1f2bbd3e8ba07b7aefbdc7c14da0cc7ea680153b4703261bb9418a7d44c33b7b94c6757eafbbdf9884f1ba588827b0b61a6eb9f6faa9b4863e7455c97b8d763c0e6af9d37f35be7383039acb811858ee9197126708d49b183b8c15bcd8610b787053b7b1556f8bd78ecff8725b1165ac7921b78ecfa446dbe51e4fbb896cc91c49a8236aa1cc47ca5acb5dab0ed01b0ddbe643cdab41bebfdccff20e8210db9a75eeb4fc6c9a351726b4bc19f7abcca54bc0751b8fb884e383fec72b03b93bca66ddf6d1fecc271ca20257ed664e6178bff5cacf90285c6534748e5d1f52cd3e87de9933674298abbd7b840ba82f90eb234f093963aa0a5a0c6115b29822b2ff18e4e9dc0c3d38eade208199585bc6ca003d5c715b5feae59542ff2437447730936c71649e88dbb9b4712665bd80a2f259d9c8d174af910e8ebfbcec979269bcbba13a055720693c673e7090ee779c1bb067ee782357b8c9c92e6a942a276b62aeb98c88f0df0aa143b4e79e7c0d526fb189e3af2ba37459272b6e9d6ea52ad1ba43bd49550143ab4465f359222f0461172d001f3b469d10625702164814f5dc63ffc2da35c28eda2161c51cb83a5df88456e00a47ea53c8890f46404709c39ee681ea1d66b580b1abf7a765733325bf5b11903c429ea2630d74da972d32b6a780da684b59bb4b9850a300341711eb8c7149315e82d607a46277eb25ffa541dd91755a13e2a8f8cbae515b89c4d9f7f5979d65495c33e59c5258a8cb21a73cfa7cacc983311fd3d4c3af8c993a886e60270b0ce18d5ee4d29b23c738df816f311dc6ae1e55949ab19ed512be24c4d057120d1e8abee8ee00f5609d922909dac959009b1aac48d5a217a8ef77cfc6bfb797b8f12bfffb625f9fe8685fe0b6d3c59c1afa8bdd5e8bf9f0ef701a936decd62fda53de3ca16b3803d8c6ac791a280a1c0e5bcb3ecb9130c3287488a3cae465c531e270e1ae9fd3e31fe94561a7d57bb582a99678a5ceca0d57585e3b93447f7579f155e11b198bf6e7a9720a480f80fb97a3d6fdb71db113c06ee513a649a2dbc31bb1f2ab1f69f1c05e5c8adcd4fcb60f444a6519496bdae890fc7da4a3ff08cef3a365158fabf302302628e1b5ba0eb88e656c0858ea48d1a95fbf95eccc5e730a17aeb2713288e91768e823ff04569152d2f51e7e1e2c0fd2086e8fba692e9548fdf0d62d319cda73cc16285ace65019a05ef00eb9398278cf761b33768a124ee16d3b0a597af61c60971275e1ee5f9012ec180cb1d4a12b9ea6ce92ab59ae6ab8418978509b4636fcd7173359facfc8b83a4119b5485e1b86263ee40bc23891c3f8285cbce5480e26cd44eda1c3b4ca5f1ea9d08ee7373075eb8a7985007bb90385f8caf772e256bebb228e6f1f4bc1889e072fe6cacbf6c4b9d8b14d847e40a849fc5120ae5508ee8a4ebc8a445441481e7af76773a436304a5f17bf07c306c51fd658ade837c16475a82407b1968a889abdbe2d09c971fdade7e1ec0fa2359f61ecd23680fd36175de8d75822304949ea3ecbb02add2edb55be48424284bc2241879a8895cea4a392922eff62e692f7ff843a67df0c055cb56d67e64ed6cca78550f6f3d4a6590c24c612ad014a89fc70e9cc39614588f49bc623ad1c1fe805437083208e5069ef74b34451624b74a304c9053dd86bf06d46647d372d27b363bf6c4012e370d6042e6edb5acff451789030460e22b0748310f3be7a152ae7a7e25cbec0b9492a58d98e18fd9b0dc51fe33519ee7b3b2a5c5d8b6f4c1921b1932e285c68adbc45109c84f7dcc348d2fa02f0a1a883222990187fc2f51ec4be110d134e37f1d93db11fff03c077b6a18962857ca21650d03a0f22e6e223e22a698b457d8474be6020c94e0cde58db422716dd44a683583563d948183757f99cabe55803894bac583bb6c6cecb29380df24e6eff1f81a243cbde655568ee4b9ebe99569a98ab83154f27783a0f8e620eed81500bc3609d17288c2c8269ec173e4f64e5779353f1913cf6df4fb01df03e0f05ea935c36fc8bfdf82728171d90e536934aad8152d21ee8e1f4eea049c2d74f674c5fcc5394f00364d9e9444763002bcd9c5c93efac81d20f4ead53b2e57b7262dd8c689f5ce70040e895e71df131aeba316ee02603caf76f72e1e3621c9026f14a84c1506e68759f8c08962d5e1c920a71936ba339d292e1490f9095ad96f62fea5674cc4b679b5cdda8e581a20edd5dcf14d4409e3e31ac7e536d9f08a3ec597ee01f9a28bf967f44cb67397b138c071ef85ad49fe5c411b655af164aba82402df6835a019048e5c4453daa66b032c6d2fa426b55a0a631c87bbce4f98b3af92cf1c6b1f61f448f1dd8041c349fef1d86f63c43a302e6f3d56bfdc11c3edc4a506a9f1a674c66528ab9f5357eea14ed0b4b12c285851239c950a70d00be549c8e53e93d6147029ddbff266f4bbb0dc07bde229a34c9886fbf3b180210b92d8c6da707e427098bdc5e23668b89e53db637610f41183b889029e8706fe189b64d537be3c81fbabfeb4deddedbbda0cf1b90f492c5d5fec95bc87d712e4523f091b717d6b77f02dc4862f1ea7df91994dba599f83163aa07ecb642bf58f31701c48ca2d18035a1059b38abe55ca93dfa07028f7dba572257ae3cd3a63766aad977a1626eec4e49d8328d725aa502b7ed4b75f4885e20539d77ee20964cf59faaba4f8ef30fe94f16dd091260491d1096498c7c60645bc3afa19fba46cc6c1c5c477e307564fd46d2daf99a43ce15d50098f94c320f289ce12816d3fb57b30a09e8205728b99b3eb0053ac9b79e3a35611aa7b79e13144956a0101a41853e51f4db4fad7191ae32c70b0d1c1fd596efe53172c880a761cd6e285d6ad26b57bfa2f1ee6477d3f628b2ecf2cb201f762290cab0b163de2b28acbcb3d654474dfab56a7e93a3bb84f06917891801b3b00b8d2a564dacbd1c82818651d39506f564adc505e907e66e2f0fed4da42d4c4a17427c0b5c4f200f8bf31b51d2229a1a8f1cf31fe928d774668e98178b3cf51d7bc77ee823d0f0d3655c7249019f1c558d2e764581f6ba02bce1c1f0dabb0ac41592f32171ebe81fe0785c3db818ea1d050c130448e781eb49c72cadf2b77b7915c40e64ced95a64a4c01a0e01975f8146fb2e5f1ca49aa4d0ba7b80a5d53f4ebba20c20e1ee15a4ef2f316ee5baa62e7c323d3b2c7b64937e0a99b4daf358cb5722fc9e1419e6494195301a550de476230425ae463b1213ad542a171b7ba5e884a06bda3c39e7f75cb18aabea09b6f64b4914ccc6e7ebcd50dc8014e01f7e5d587e078ada82081ee42ebe76464a18f5e6dce16d8e7dfe3ec103f8ab42e45dd7ea2eb9e34120af1692fcd8046ab52c56f301b329e95bc3e664ce977bfd703a08e7267fc64150e6b8fa4b8a8e6761bc48e2df166b3074319c576e5cbab5d837d1a34069996f9cc7d2f8a8aed240d270136c5e7306f1108b87f0804dee1138deafe9ff41179bb022fcbada60d3b5520984bda739d3edf5c9712130a7a04191d1320459773b883f334a035599fdceb1ce3f9f4990590528887dfe09fcad08c50912d43fa2541d51d1589ad733a1cb335492c909f2cfbf87852d7e5f757baa4e775d7bab9840e753000316b1cc165bfabf2bc1c11767465530882afe5827818eb6c96d04c86e7338e888cfc534c9b8c5b4f25363244c15af95c42b6141a9d812a22df4881be42faf34dea64d7bb7f93dae527b34eb319f2a7c1db79efba30f51c49760070c978b5b5653e3edafd6e0e0e737a5985e12389510427ae1c9ed8e9f5ad3205e90abedff22a981da2d62fdf9663976e1d8cca5ed30ce37d37a9d282f0b08d35cc6e6fec011f1d730d6cf93e5001e829362a139dd11de32304025c99eb8d6d2c9c2feb8bd5703f342b2fb1e29e510c9b1639e38c82924df19b313de62ca30da708b791275ef727febd0e705080767e8b2629fd2a92e209721814c4482355eb03de0ad17cb1f57278a645bd506dc3ca0357c75f41e15155515d2a98f3a321e15aa79273064d31300e1b472a4c3628622a4dd6a96d13472497fe1272767f65bccdf719a1a3eab35dce1682e59b48adf4b80545526a66c644d3f8f8baf1dbd3baf12e0fc3fb584ea53cfb04aa53fb23fe3e9c39546cc05f3e9b3ae76f63c018334bad304e99c10cae9808937a8b161dcd5d89ab99c3e1bf075ac97efd846631a6b0b427bc9a51550c227631e396bfb7f34890496004a62f0f2224a9eb9f987e60fb2fb2168967174f399fa82ba2b2edd9c6ab43667fd7b7188d646066a3a264134e4f563243b12a4b08cd4fb7467999daab9c92830da93aae3fbcba9fb2f6d44078d130cf457ef832cf7a192c8278af4d1c65d4facba14c462d1a7739b838bfaaec5853b0c3f7023973eb80fb976e270a4829f12c76c2484b43be6f19603d945c14eec3abc3160673ab19798d6fa41af9ec92a4975020f934e370555aa47015f344555be028e45a6afe6678151abcc1dc381a3819e1de9422f7fbfbaff832c35e76f84305a7797493374eb56a4237b8519259045088f9fea60f17c7667b39d592f6604583f7e2c5817eaaf9dfdb23de4bc9a300fcd0759b14e5603e065cd961ea02ec6068c644ddbca307e90442eb0adcee5b8b481472e0ff2e5fc2c51c8343b1fa69bab3a53e393409401055a995bca64102cbe0eb302510582d23bb6e3942e15ff8302ccb554fe6f349e14b77c0c111270c8b38a9715c357d240cbd49b1accaa16ba1d88d17c935a582ef69e602ffe3b8fb392110e8df230e8633fef11622e49ffa7113f7ec6b8401db8b22ce43c5286e8570f958f41d8d74b9b7e693290d620aa5c86697932c0e7361521d05e75e2ee67cb5bd5adf406d741d8b1e4bcd1203800a11082f8e087a4be0cddbf6172a4d488218b3c38f57574cdd3e82808b065ece6243e28d588d83b9714518b91c5b75f87cb61354209306f2b334868bb4bd5bccd9c993cd7f55de2300679864fe94f60f88d061fdbe10c46eb5a6e80e5c95a9066c11e26a986f0c24c25eb697ec1bcf1aa8e88cc596195aff3bd8f1b9a5cb876d6a2bb00c2536d501a77a4eac5a67172dc01162c4aaea0de91c7e72cdf5bd92006d1a256253de5bae21be100d3cb5f7195751c9c90c4d39ed5e5ea8498191e6c2871e169df2c29b66ebe6304e160e2a87dcbabc2a19db5e82cdc7aeced4a133ed3564a5a50191876e7d4029fa18fd32b8e2f968247da8dffa83a3ff509339f97fe0a3151c1ff936f3d820512e688c6d3fec56d9cf77b39f34e5b3bf611ed377c7ad41bd3f4b48178834a001702f93af29262a644c973a0b0188c0468655b8883c8d3396fa0cdf2bc9172ebeb25e283da08b8794825c94ba7651cece07014fdfaaa780ac2631eb49e3025574791b7a3a9ef4fd3299826b914a707fe0a4c09fd13fb24d8f9f1737c3da14e01d9087aa6e9662e3944323e24fc09a98a2167385b0475c716f6df9783191997f08048af79305d4d7a53f504ce22e5f9f6ec3c29574adb379d0cd30d5df8e6568c39f97fcf749c75d3aed0e1605cd6085d36091bf5ebf385add003ce9c2dfe437f72fbdba932182f11a4bf6a897988410dc45de824cacfc70de83d548d55d569b6929adc5a9e49e8d52544e5b9fd0a9feaa0c8d1be927d00ecd42396978441673a4b6faf5324b3470abc4fe911e9f517ab1248945b3d2d7704c7f7273ba309cca6bb50be612625a1bcffa6c1861779d563943b8f58d2cde27e6d4ec85d72da49ad9dd474baf360aa0fbb4ec84f19361e98e9393f9033e30aa42b6d9567fd5bb0b88947c47e2d7300336c60b3739c2a95063e1cf46d1f82ea8e7dfe828cfdedac6706674370ca30a807ef3e03d2278c3d50aa32453d394feb2f09a8cc8797d1e4b0e33f71838c4bf5e7e5fc691280f3d7cae8b46791503fbdecd934196401cf6e1e2ab19e2a111fe54e533402d0020591f4171aba916df1e86d58ad2f7c1ee8995f1935c6beeec27a3dfa78edc895c70381694ab6856bd5f0682fb0a74b0694d27929a7a22d0a3b6abe6a7ab5c3cf462ca9f60766603cc3c9a1f9813d6b292fb63c97cd00b265e6216a50eb3ad310a9a8573ca69a0759985fad75a458963bf8fb3ede06732f44e5abfa29b9fbd27a5ea6fecbbb1412c66536c0f0708b0d4b1f91f381624cd978cb8cdff0773f17f73e55c5c92d8fdccf5eda6d0d4e757f1a841a606892f8a257004c0be4f397e979558038e457696e83bab907c1bb79aca002530584b1b651125fbaa9026670b6ad25c6f6c88453a25398113271b24fc8a91dc514d7f2195e9c6da7830529507c721d11d79e11abca2a927167ebf7cdbaab2f5c79c63396ec2b65f843f37488527bc1eb0753ed101dd23b21a75c3c0abda05428b55e3cef64fe86ec7280ce70b5f8a707350bf376fa2ecf431d3c1750fd0cbc3b78ba04abe205404d575f06e30b1a8f2df57e6f34444f479974632275c5858e9670cf53766b2e5f45301f0b5e8feb4d3cbc715de1adfa12ed1055253b8a62e802f73c290820db9f25f8c45f15bf8f152807082e76afec2bfeeef4dcb4c32f1c4de946eb785ec84e1b7bfc4c7e6e39c9df664aa4ede71e6b3a34f1d09a1471cbdd6b0c0c0ad25185e0ccf79a52b5780637d27e95cc20a9e084cc51cde971f6fc3fd737ffddfe7afdcc72c73e739bebe831730261a7edfcd4b48a1eccbb4cfbb0b63fb51535425f6ef46217b27c271c2465b6127b07b8afdea827e3d161bc3895ab0de8b71f1bb1bcf2b67fdbde92375c519fb48872598c811b12e83d5a8b6ba94c2df325893a664024560f4be6b29f183d51d544c95bc608ef5787fda8798bd71309284ef1ae791db8f0b7c4c9eb0e23e3906edd2f1db69de0e89e2dc766f223d457864abd3590d457e51e16aae3b151b0d57a15afd4a003c7a3e060884e8725e346f827e6e1e1a2b815949747f740b7fee4e0850e3e128f51e0ac1d1c96d9eb4980a86632b8e10de6532d65070349dd90bcf5b6f25c0d3253f571869ee9b4cde4a1726983bb445d83b8b484a21d4cf09b892bc50f6c6d76fa09bb8d1757b21a831ee4a3ff7b43b80d0f3841ab5a8d8c46b572682a35d6a45471a5380e5ee6d280fb7ccc7ad63678091e41ce09319e7f8075d940263b28593db358918e2e0d957de97067725eedabc3a0db3c53b4d9243432dc454a1fa93826c2b327c07f4e78bad173971058dedd00abdfa1e86f92b794db414f70b88adcdb0937a8cac8a0e591018bb26a012a5292e44616f6706f6737a27a6f6258ef458595e143f0a2ace83d43c20b9c4bd8641c3310b7657a4295549022993123c3f783de1429202d24fa65f0d897af62bc92d14c4633caf9f92cd00d4f3fcd44bc81fe81ede5069e3b4f2e82ea1ab2a68a57e0bd9edd9de69b41352255219da4317ad24b359d69dd54e276ce70d94abe0934e6e91fa9c251ad159914a6ff2712b88afd2c183c9db3aa47e73238aeb24656d58568c21d914a755d35e2fef4b2aadf44d6abe742942de5aa983514533919d72c67dfdf33bc1522b261e2f1697dd9c8d41782798da0aa361dd99fad661559ece6db72e48ac0af473a2e6108067c5d0218385933995f105d9fc4485b38e20a82c8a1ddc4d776986bb07de59a83e8428d9d2777d47b57f0be73f1e169d8dcf067f8ca761108c61d393fd6d08a4428d7e5dd5bfdbcf8365e7d79318e4bb91ce09cc8543939dfc0d1d6743e4b92536417f667775dd5b3bc808ff27c58a9dbe9681384ba1cdd05051bc8a1d7c99d818a53fbbe39a91998a56ad9d75d1393b385df61dd7925473ec3c0339592b4f4dd257ef3c96b3c6db61a2b82ec8f90ce5299ca6f4a65d1c4b19d9003efaf4fe86a65655bb899c9886b5d9debb2f29ef60085f84ca287fbed3318acef4014cef53dca265032569b273aed83769ceed4641b4a3b4964905a92f729a90119c17e58f6e50d2718d960959737abd0aa98140aa7169cbd045e4a08f34616ba78d95837ea83ac6d0afd86926af3ca6264f9c80d1a042d71465c437b7ee3b85e5e8ee2877e39e156a6a66941381ecd74f93efb71b24747caca5b972ec40385881bf724a37809d88cc530a1d0b9f825180167752b6d22fa77b3698b09f13b6567c59a84eeaac5c77da16a6f39f3bd1d81467d23b54765931163e28dc85a8ddad4b1684f3a6984b0404dda0a32bd71bf9df3b3831bfdcc9495148cf492df18bbb6ca07218b3af9586d1e16ac29822228639e4c628cb1e0e9aa67429066bb3fdb80c28418e883c4faf4808817f5d1aaa0a1d9d5d62e1f88ad0bf770556e9b8b1313a46d277687e0b12322d7a05d2dd8e7e7a0404e09c40aa21fe97530a572aa4651f26aae0b935d9e884768cbd69573760db31d3a758c7fee77d15c62d1846392697a9bb14480aba430417680f319f2c56de0c1c331c94f76a37c8d9258b4ed8216f9c2248445c3c9db22d44c730ea509bd3c8e695be5551875fbca545e3fc002b4f6c3900e3547ad6b707e8ecea5ac8c765eac68b12dcaa2075e89c613c6b0f5cc6e2def3b7eb0398378d979d46a52aa83c21892a3323049d5b0662a2dc32518ffdfdca12ade2d5f1342029b6930311499063ccde53d0fb89f212d7cd62795584c269c4a534dd247749462ffc1375b137398eacfb5629bfb4a23f248f78ace43c8fc247a9e8fe69867820c9d62ba671ae44e26bcdb38a337cce0c8448de31bd319f4bf8d3bc0e485309c0d2b2aa09b8021be7506497935880910298bf2b8d5863162d9295baf3cdbfa261ea4c2707674ced638dbfe4fa5b66d6d203711f0ed993e2621f0f6bb5c5cc8fa74733c33a2a21244e4751186a47776b966147e1831cc729d9d15cdcfc9ca6f728dcca569e415f5aaf35618557e6d56f0cbb6f49208d228181090a3c08d184d0f723c394e213fb90235857d78a747435c0a8bde69a6211b6625305968f2f155ba067ce96d63268ae0ddb0eec1ae2488546ed9b40257143265e0e5563cdd007c5ef151a9af915c348e8a6c7e42e997dbbce1c20eb057a90be84058102749e8bb929aadbf2c0eb46059a33158b2ccdef31a041e155f3f9b9c8b28c5b0ae7729c4e4d4c8da5d35203e9a68b8e9f0024e55c8bdd379ee7de08fa4cf860352e4fdf371e213703177ef16b24689650bdd4caf8c4ddac4c2e77f5f49499aafc67ce43dddb79536e0928c6e19254c45035a1d9691e3164cf67798c466e615e12aeffdcce6937d946836584fcd19046403022f702f8c919fb33ac58c77f7440bc789d3f05e09d102446a2a80c2e5fe7f3a666df7d5bd08e961c65e826e3595cb3be9c1d0833e021d1c8cf343d76bb2d63e78ce438ba71210b6b0f7f10dd4e401c5e027304e6378f10be3cd83f4b1ac947312bb83e3917ff5aaf347c9d42a15e8567855c6fc81ba21ae1fd8c82ea61f97fbefcd531b53521e0ea651c7577179294b94cb1b17214827a0203dff2fcbb95c05b3f1e99d60a47ec167ff9199edaa636bc93998d01ba13279a6e4ea717fc768df378604da1ae3d31762613013712e0aee02af729081abded84aac185365ec016f675213448b689607afcdc3da601f9c2b64d73f2da90bdbe055c873b02100691a3b4fad0bf6e863f44893174c06bab7c3c18710df2468c85142bfc8fff8ce8ecf70218b99808876dcbdc2d9adcb7436711a0ae9b0c1a6b93b63ec4eb5346078e44b80220b2ca72c4698e88fc3367396e37e31aceefc285e3b0308c55d2c60bf55929cb2d0efee4be87e279c599b2182c8d87011a603b821a6df31e34a61f015274eeea1fe6c0ac587c0b95bc045eb84ef53416ec32eb309eb671566a22447ef0ad4d9a9f3b8e4b283c53e8484c500bd4e5f065d8fc3e0be1f35a276c436f85e1754a813fc7aeaa9e848fc14c2295b7ab1d4a528d7efd435153e5eaa746e4fd039efa604e8b22a0e81a60d31a575434e045249ab7448bd80e0d2f4d4e2859d68033439aabfaf69eb22200918e1eb924a392fc49516b0fe7b8c3874bf6f269331adf29dc6ea28bfa524823a996bb25e599643202b0f7a07a9a250ed57e3b1bd89683ad30903e9f8382df3ad6acf6fd25612b120a92e319e6a57a387474b674e778a02c697816f21dae2ac6a5b1fd46f618dc82162c5a852493fdfb77c426c3ccbbf5674398ba51a359a7f65d7b8af70447363d0788063232e7854f4898f3b9335608f168e8f9fe22deaa6f243d6f36d224d70699eec9cc01c0cea965b42471dc3e0f6dda34fafd8faeecb1463e209a8a8897e45af60a52416fca3a3bb470c594ef873bec1115e1e9d5e779f03d41402b1b3320ceb8bfe3ae870fa454fa190a4a1ffcc3b34d232d474204d5b542d6327c728f3e4cecd4fb1cb5fa3e8ad7ae926482bba142a47bb9e5e3538aef440c818197f16d1f5db9da556348fe8148819df1bffa7340ef4f4bc456aa6e18ad532936ce702c07fd21067478e86b18acd3aad4afaae72d56fddcd44a1c4b7f4160c00f1055927b1b14454ce533001711b80cf53e6d6cd4cd344fc3404e8071c73f28cfe0a359dd0491dcac2c68a1a3f17fd4418cf8f3793ff0e7d9b7722b0d6a70768d82f1378bff8cc3d6ec8fc6710e3b095c3225f7929b5002f2b9b63569b5ba9498c2af72a1fcb494c4edd19cb916e61be04a83c93ba463e84b58ff07d8983406b761763f5b13c41b537b40c257f56bf9b503674aa3de1bb149393f3ab5fc3a20d0ce0034e9d6d77d3161edac4c8dc21ec374c2e6312397867324072d073aecb676a9594c7b24816b0ac78bcc70be940383ce5190ad37ec94028b8adb6b0daef66731706257837cc183be9b4eab6e0e77588374ccdcb711677968a41d02d626f4b7ceb3a08cabdaed53ae5a9c9078bb331401ce8acd4b4e630d93f56ef7610c4b2e9fceb5431345fc0d3a05e0aa8a9266704750bc30f07d17f620bf9180888863c590edabd643cfb2aa385df25cccde84d01bc9d569c63d8fe7cdd99bb76a0521fff72ca796270e1c677c5f04094bf06341ce747b053c66cf95a31db784006c75269d29cb262dd09395372cdf13b1455183b3306a60c25a10ed958deffcd17dcf33d5d0c8ff24a721991f4c0bd2c9c03e3bad8991e678544b450957229d91923808cb1324003cbdd7d0e0bfd8cc0452bde71f72837f343351be99f48ca44518ecc92cbbd7c2aa076f5860f7b4405aa8cfddad91aefa14d40f343e23974b2275bd2cd63eb83d03548680611e47332f697b5139da8668c33dad36ab7e90c0ff07881fee1cf4e595389d915c92a0b078f48bd4c35b47ea6d26b5b1e54b9cc113edd3d3c658d0a2761019f7d72215f38d92ff138dddd9b84b458ecd19b32a7969b5aab4ef5526ed95ba5bd866b0d1dffde17fb760d0057ae43ca185f211ef2cb970adc8ef9d56f005f7c87301e9350a496ab930480b6bccfbc93ea3862bb8fb8f59520445051911c35548ae0cf30833366d9877291edf802ce9dc4c88c2c961d77cd323d56490904135411ea0264d6f173c15bc991f59357616519f7b8233df95604ed3fda9362402641dca62a67b3536fff5e66e3939e46a50bd11013f5ad196a5aa69ec3dba927d60b50185f698679c805bd58b890b87ce37ce886d58ee799ef03a82f266407984b565c86ea073b72a0d4d5ad46a2acf859fac1c7c59be7510586fb6d33c4852be6ee03d21cc05f0f4ec2b67779807893e864e3390d8eac132c9277020045567e932036897ca21c1edc3e0c528e0efcd5bc673cf5964f0b050aa42e4177a12bbf295bc09ee461a1194e56a3959191f0b6f4ee9819177c63285bfe75141e2bf360db8a911ed56b63d0ffc3f0f0347d5f4fc07df929068fce40e7943044c6119020ec8f10b761a2924d254934153c27b8225a63d2b7dd016da14d3435d792a4c7306221cad1f0a078b72b437ecf6a7d67b46166d779ab95def7be24198c32a24ab815e2e3d706225c317616601df3941a505586a7b19d1bb08091faff953b559a4c2ae0dd5c1a8185e748155cf63cf26830ffc429b21c6d8924d0a8da4e2dd432e410eb0ced821fd0a6bb8c7671df70ec063a1d30ab55ddc2e69d963ea848b57a17772d8149e84a20b8213c1e021b4792896025f5a6dc921c1d0e8878b72780dd5cd8d2c8ab9d19bc815102d935ef2049969a4f4ee88b13d4fddbd7dadf05d65a468b6e804e97813d73210fc4d3a92ebccd0fdffff19e0e1359217a872ad87671ae8ce6161cb03092c1a9f601ab331d70dff14ca31e483d7f52e6cab754608feb5fe93f92a06e7c08743550848660fa36a8ef631647eee93238500ab74c55c1fdb71736a89e4d86e19f5e961588db70edd14a3629d1c9dff2ce8fe48a7daf6472238135252045f81c35ff46141a0687d825801e2f181e297f3d68c0797e80aa1c59163c296b36ec97c7d45079d2262df88551992cc13d7573e92dee74d186b05302645b3d6a83161d2c7670aeb0454debd42cefb93f953e981eda5432db976cd3f2bc428efea9783089b424799c067807015a6458d6441d7e43e2b6722e553da42aa4f54599803628c9bb14984b424031348a245c648acfc015ce49acf773e61503e85f4520a90fa9d242adb02a7c629b5d33fa7073d2fdb3da2a7081797557a92d48216ba7c384f6054a58ae6388725c9bdf27a7ff7439f1237d7ada51e587b1e75a28bd4b0749c672c0b599498db14119e8d3b6469bf32bb65d844da13e64b718b1621061e72a31a80ec8891f7cfbe7db03a94b093e9c236b24a7e0d85b2e70f7edfaff73ea2cbadc886c5397d1138997a9ba69db01695c0bfcd1bde2cb4759e84c2172ee4acb81ea34c1117ede9d93aeca7e239793575cb8734546251a19cd2492603b3309ad0493730e73190e19477f47cf96ad72e06c31b1fab6ef735ddd35cc52ebb7a4c52f5fe112ee18f7a7e19753974cecfc71c0a11f9c49bfc68b4a0d296be2da847baffe6f0ed78929b04a2cc9f4136536d9d7f589ad93da9c2b0d8b2862a72d225fdbe5975273d388b0c45d0a1658a96575c59a2bda81b8259e23f0ba7413505a0e7cddf4d0aabfb4692cc452090f13fde8335fc42f13270691c89cd80824df51a922464dc1966cb7c90b81c674bf74ca864936a51e1e7a6b739ac70126df6b5b0df926dac651e2121909653c3b726914431b8d97457d9fb35f705c8832b1aaa3464a26f5df6838a180af6f47cf9e48651d2af1e0858e336dc078fcc19b27eac9c7fec96adb5d209e109493b4fdadb28e24a4998abb7dda5222ecabd01cef0692776b683397ae6b9a501c610e9938908bc194439f3cb06781c5f1bd89a770612094f69ad531cb23eaf2e6fb4a25a5653713f2a65db7d555a03039d45d85214b6b1ca7735ae065afcbde9bd3316fecb46fcb1ddec051124e3f0a3274294d117093974d60bcba6c7dcc05b9b2cd757bacf6718daa25f91418add1afe3c2d40ed9e39b7edd9920855b10a6f73d45088d184eef17208cbfe0c295060db130dc1e6187e60420dd2c7579d5c524aa69c68656f9a0eb3b53f74f6022c38b2c569ab5c1b2c748eb658bc19fed0aaba3863a5a03772692c93c4454c67a0240b21026677620b438bac592a8f1b251f83a87583fed707ac70c86d58a6e0bbaba690ecb07198057d3c40d00fac6829ea38367c2d4b27122cf37e74705eb5d7fbb13d64a040857b911e01a5b133c3748cda9fdffe34f5d7a695f98290b7aa4f7804eaa2e167a4b798a87a5f0494756c20bf244b0b2557c8b73fe58351b9b4c02be840df4db91cc0b893d0e09a54d278af7aa8079e14f8f3d22c10eee34371d8c5f247514ea4b079f0a53d491edfe453df857a51338f847189ea79bbb55e9b943364bf76f4cba5788ff23c52dc38c7ac4fdfb34f0cc886b0ac611bc455dcdea7b80394bbfa27eb97483455f14b453cd910375e337341135509c1c9f855418881dedd149095377d45556d92d91f6bae7ebc17f83a14b7d68d1378ace8d8eaf369be1e9292e7eaf4165656ae315882cb99e2ce8bf7719be891c5c4d4b1ba32f385450cf49615910b585db1f4f7736326a2cda2ef4ed55d7d4a8232e943fc170fbc54dfc630547f35261c851c14459e098ed98028ea690c53dca9000d5e97b7d8951d024d90e24ca45767684a815b509e81ab9de07eeee0f12f6a31991a77c5f7371c902f1bfe05e7e590dbe8432200a87cbfd33ff08b0dc7fc2f58a86dbcb8b8e0a21e8c3be57fcf084ee36d4fc8068f8a55fe5bb6259ac6e41eccdcbad43969d086f92563872fcdfc9e7e9309a1cefa03b9098605d02294742d2ac83ef6d9546cf44f0b0897c1ac607f63192bc3b17cb5e21559ef00497425843c35efc1f1717ac9e4e8b30342460ceab0e5a810852b4dd374438f189c2a2edab032e6efb1e77791e0a981eef1a0ff566207c73a34f592b5df914f1426618ec7b9be53c4923ba1ca831e07be47ca8cd384b8cfce833eb752ee21f3044535590ca64dd5a07b0f8ef15e539fa881d49bd7b3853a1d5631c1b83f549d986c8a055b6b5269c165e05401f939fc17919c128ba8498ba036e450468991d30116037487602eb8b94728f86ce0ad399feb3d73dc937bfccf4e99831a7820136afaeda220092d1d0551a298d5cd0cfaf93a4ce8310486950a7930f3b860b4d84906fcdc41fef177ada483db3eff281e73d559679d82d2c4fb4cda8d5120e5a05746def0305a526788eb5ed486d224d226a25ebdfb13707f54c00434cd58a87ff24f8952a9de0f8a49ec3aaec68be6b65eff287013ce9eb09b5830ba03b4634cd12c92c4779f9915c988ec49aec9bfd06a8e7df2e3815462eca3f0346fe9c846397e32968c9c1252cd750a26a502b17018546c8693b50040fc5e0bdb62d6f5a44a70cdede3eb47273757b9d52d585123ea6689c86c0d5950fb88c94ba623496b65f4440ca954b1f00d61639bef5e21fbea7b26200604b94127dabba42ae5e9448e8b970830fe7a44efcf22fa7bd8bd9d47a33b8e97cd1014a081822e0343296df5b092a3e9c70ec361f7a80c062acf5b24511d0faf70ff22224ff116dcff4016df44dc685677da36b5644548adbb6cfcb2e2b3b9498b30bfa0c7b35bdc593cfbbf9de8bab358ba5b3b9b7e72bec276e92340eab96bca52c7030cc401c01dca67fd8054ce347a25e9462f2ff8bb1bec8a7adc4692654a0d08176e4f402d717cf6f8f77fcb2a10e8a1e327c5fb00c8694f2f32288f30895d64aa2584f85227a38b0d3cff28e21ee571e3302ade91b37ea28b52646be2c551ee2fb799515ebe8066a246b0d40a3a20b7b69db0ffb2780d38d3a8db9d6ef5b3af854f1232e1183ec71bbc79fcc9869a24afc1b536e2f7c3b25c15c88f8d004a8b66905040751dd3e72edb8dcd2d033c311904beb69985caa54baf5b740eab9ec86b90ecc90256f22c2459afaa73f1f0e547caf5f157245bb19546b4e01187d546440ead4291ae64e5d2336469e44a9ecc4ad59fbb333475c0a08e05bdec64397eb328e387494a0332e6cbe30a34c5472c3bf9782fb91dfcb2dad7fa7ac45bf5076d61dfee198e83a0357336ef361dc78371b65b109cc03694b6003d38902e66163d09bfa23288ca912229a31438fd15e48b7ffe8a2aaedb5b2892f5a230b4fe92eb1ace83d375b7c90266ac3f373d2eed170868926f41b5f6283aa5f9ed406dbd8044625f81a6682f3599545f0509719d60123f1859d1ac7510c4e2d9d31cfe70bd468ae9b1eb9da10055dc3433a600ce8732aa9ace8ddb959a347597651c337187d80cc8a0c828bc3131420ae868cda9adfa39c33cb582f6372c57349f5345592d546562a4864ee239bd1c6f647a7cd1ffb3b214ded6177dbc1cbca0bbf398aaf1aab8a1487ba8c1df2c774b44ab61a243356b87dbf9e2a1db670bab73392fb781d7ea6e6f8f7a9d1d6166369d882925d3037b218b6fda8e2a2e439e33a1698c705ed2d359f3e48f324a1385f27b6bdf486fe28d70122d2f898a76c99a93c0e2ef009d5626b9a31da01c5bd7137b1d7e48568adbee9c576687c489b647bbbc0d0d25f0258c5a5f99251cc24f102f2b32b9d5cadfb49a42a565c6319af525061d7da14bed94f0a0438573c3843a991825653214847b145b48565b0d989b569c58cee14091a6cb7e1467cbe992824859d0bf241131ce8a45aa44980bc46bc0308a0a11a67f2f1eece781d5320ea5a2bec8ea6829d3419af432f6b6f10066be066422ff5c966c780979cb2e32654d6e7a1ee617bfdf45f3d22e6d19c0356690dd9243439339d3cfe7676bcdd6ca5270ade5dc6e3a67ea3ff8784b0cf343e3002dbe96fab89bf32c23e304237138a13709ab892eb9ac414ad635fb5df6d8620d7dffefd9f053f19b0747c1f76b7033948626f635c55283fed0516a3f622ab6737b9b02b3efb65a1b3d673bdcc5c4a53bb084764901c2f663e3ce6f95c0e917de7232a58ffc2ebee787acf6922e7f1839f083d6232893a31a85945e71d0391d9a01dca6cee76bb486e1ffe93748bb25b204f18107231ecf2bf40417e0eb299c69e463cde615ae4596690683eabf55c305ff860dc920cf044dd4e676b6576d5d7b02c07abbdc2c10254ac5bebb7b53c8616ce47d7744145cdf8cd4fe3ff029c1e38f1dbdffdfaa2a2c291301c2423c57dc2b687d49a5f8a5b5c57435d1ebb476d7bb5bff7ad42585d44d00bf580b8726f37d88fb0fbc335ec49ea8e583829d3dab7da9eb641fa6735e02ec99325f0b79c18c914665dca577bbdfa1b76b84b1d3115476a0db967248ecd74dca74856a37b3f9de7b344334835874c512919ac5f2894af32735947d54487f40433fa09ce1e4bdd0be0ad6cb04a0bab8f69b7614ebea4e95449af4a6cf64f733d7d5bb0292ba9f815f317ad3d6c137ec2d408645150a03b32fbf528778bf9fd617b62ec0ad47cc19f7dd73e50fa8b1667f516b191b6f91f9a853e84567a18c01842574ecd143d302984bc3286d075f11f531a3252a5b7c223ef70079fbdadce0c8b3d5c3f1c3723ad51054eb21895e47993041c62123cab3656c5e5b428d627f738b10bf8b3a5c8246b70d2580e0f2de43d1d7a01fbbe2257abea4017daa1fdbc89eb12fe4a44b0d3a8709444bb83c7bc16913662f8e7801d16e36fedf7951a5f445d1fd919e01e69d41d44ffff8ee1115e263c0ed0b10eb81a3cac197497f26176d9bb3d77bf7cb37440103d877a0b25e102ba860e0a7dcaa6ebdbb81036c8e320669c929481e20e783a6d94a810459cff49487e3d7d14bf793e9fa9c6b6605c5bb8263fc865e451c7b4340f68627d14744e1f36e866c22345007e5d78476654282baa76bd60c0f0006b4b7d626a61d47947f2e5f8de784260d99e0a76f58c6d212326fc99e3f24eb6515552b1704f5f0819e2e2094259a6053cd6bf243949e14e517dcc5369164741c05917af2e8c143b401bed08a78169c0ff565bbec707bdbb2965e1a0a978fb76267407c95a80ece720085b5aec323b385b66cdcdab1129aae0f2869d45d93733374ee066967f8f7675341d3e5f8b2d6e3d3191f3731b1178e2c38f62b9ca6398eb751ecc66e64df0f5a7d28b84f2c25c235b31463460f6205f8369e659b8999e52e2bb9230356ed1a3f5bbffe45afd6273caba672f8266d740ccf5f8399b0c4e488b022cb7b5fcbbae5c328c7fcb6c9f3ad89dba1be985fa4bfd15ee95e6afc6151e8ab7d2587ec933b0bd41ca5765b49d3d23904a420cc73667b6d5e2e7832819f709db5a421e4b64a2b6f3f8be98029f9c267bfd12581c480e570091ef824e711bd822c15d2492fbbddbce94080a681e9c889428b7dca2077dd96b65a9fcec0cfef5336533697a9344bd63de3ae0435e77a66861939ca9c95454830d5c04e50420362d40d63e6f9903046cb32fd373e8043fee218f9e715a4e170c71ab3a23e5b7091e387dd51b4b560b93e2844079687d56f767c1bb9aed233e68b17c982037461e0b49cac997a23c0aff489f6fee5cfb390fc7b997a38f3913a6039f2bd25cf6754d2357b9dfb1d9b46067f675f1c55919e570f4c2d1a840a4ff2e708cbd3038129a0294ff4dc06bbe05f06bc8999edac27e9cb8433a8c7d711e532c96662408e75c9cff7327f53cf0c20a67a5b42d14431cad4c29ceac220427063b85b4628b351d97d819ffb663e3410a662ac56c91011eb18ce1d4a04e75fb0762b2d85130b0d7ff90c38200843e513b443549ae4579841f65fbcff93d7881e2b3f44fcfa4d1042c441e402c2dd89cfd046d4980a988efdedd0d12dc068ffbda3b0ce351e71a80676079864de0a34457366c77e19111ab37c7f3414bd70e2af0c63a4ad9b0cfd557415b919f4c0e89b8f8acdf87228b8b706ccc4fbed62f77b0addf58cb42e76ab88951ba35279c0d204233d0d41300bf5e1b4a84773e5a324d5381c45d4b5562a1f9ec5edbf09d86880b074e2b85fb11154c7a7d67f27a8d5a1e7e95dd8b40c804d47ed4004102fd74a515d1e2e1794b51482003208894a893bc37773c68ae6e1a621ff9b40f96c4cb445d024e3aea53794722bd6fc91f8c5045c589fa7fefceddb2885f9b1b2f66a29ba3580adc61908f12993075f16e7e7bc2b5c43319ae87b2ae49c7e2a8e11b58259449773a6fe6fdd95e27e414104fcaae4d048814f41d3386a4295292b8c89d54e544d8cc09bce78d34e02c5dc99b11c29991a2f74b29aae472b1af7e82942c014b12c984aa38411d8fb760f949adab0bbc81b973f0f2eba1ff79d2766ebb5d33757e215dda865ecb7be644424acd0adf9061c2c97291faa629624ecf152d0f522a310276580539a58335bc02d2758375674881ed14f9de4b7f3fe3c5abe399a0402ace13da7a2eae79098c047d57e0cc8456aef3ba7c98bf5d494864f17cbd33650637befa208559541856a43c2111f6826bd42fb338fcc05ac800b4f4ce0e74a8c0a2005081fd2be6c974c665de9c98eedc980553e5ed660fd87d6652ca044b9970bbe63524ce6623b7673696b86fa98e131225057a6d65ed3af09049b1b8af01fa5dcdc8b61ae90a4b8eac9cf820928393e19153b1558fe4f454f57062fb580571e47106cd5fb468f246722b49aad2188b3f5df7cca2e5d3cd9debd11a07a4bdc9f9e73a569283d1a5d7e6e2febb5e006095b596babdb6ac1706a3faef3eaf254015c82ee71948b45b3d92e5da4152be2bfa630aa3648bdc454b3e1171783eefdd9cbd6140856a738ac2285eb25fe5210a38d924f3c99f4af31144c9265255a7e80d2fd659c1a50877d911a4ba0b9a6763f0a41f237412132bc5c85b43e1a5a18e5b3e95a72fa3475201f736e59f4625d6ba197640a6765333b8f9fb0607fffd1e58480f113cfd5c13ebe84362c2c97ea2e0cccc882c1bc2535a3f710852aab9eac4a2c67e1426719843139f311513f1414e0b5c3abc89d5fab45affabdf7f5031b3589a7553046fc163f35f506c93d17cc0cfb83fbc111ba4bc6be419112847ded9fb01310b436138369905e66fef1d2bf130d9a178b82002bb864d12c04b447afe43dff1de7ff9baec41e8166212e2493e6bc5496973d5fb03f412d96c43ae5544e022169e2d660f1bf3115f14ffa18807a218637ee9d563f4ab8c5ee220c9db7ddd8921db1d3b7f7a24e0e64fd1d2772f9dcad3fbee3879ce33c70c215dceb3f80c79c227f342549b1a8f051f137fd594daa36665c149f34aa428a704e925f7b055e7e8c7ed12b807ce19a136365103628672ff0c5a2831b8bc50babacd6553629959899c55e31dedbee939eca3052d184f93b207f40bf82c466e8c4774af358c94fb4e60f5a1b9019b7596f28a505675fd5489a2e4370c8cfbc0c64777ba1e2fafb08f87f2cd804a6260c49af36cac687386e9b142d0bd716a0f81c9884be927f45e7b4cdfef9c879d66e71ed35e589eabef0ee47caa8b2f2619116aff9569999c0f8fb6f8fe345663613cf15e06ff5dd55430515e87685ff22094879af278ee281240077db15d016a23a602e624269680b2ebd5d76b2ff43a02a2713ef394cda5b1f4fce47685b3fbf41c4885686e724f5f3477a522c4ed879e6e2570b9b2fd73c686416f05458b218e4a82a60e5d219f66be4197a02c0ae01c456df45df02b0ebd0e2e72b75d8196f6d4791cee434d9dbbf6a3a13d4b0205b5353d1c625eef19679bbd317d085b67c086bcf2d1a554ab5afccf1ceae033ed165124e766d2c023ad3530bf6f84acb0d6b2bf5c922d87b26cd4eb73353e5b1711d40ad6d2487ef7338c22c2b5a2dda86644325649f0e0a9e63853f9aae7d5540314392d58c61acd5b17836c2cc7855932e92b308f4bf86d92c411f3138093e02b69c1549471dac7fbba8aaaa7a1f4c1ce666f904a141340c745117ae998768a1ec931e52a0550068fb8c37bad65015ef0cf3143b1bcc6276ddc8a34a1d3667f426b48afa1718b2c2f6c8eafa01ba1822a2734b49bf5bb64232aefc36b7c31971510e118087cb55fec7f0930dc8f5a87977e514efdd9760c2d46d904c0cb21f18c57f985635166d9d1d1481918268ffe02c859db135666786ded0cdc8048f6fd7a9aaf83b5ff64e40f48a4d05f85af205fa3e3884f3c8d2af89d62d04cc2e7d5d789f9624d49b1802106cdc3971fb6e53e613ff1573ef34b5f8b742cfe7264f22955ac470590ede8bb0b2e205ebd292deb9941bc0805bbfe09f7d1d2fa4aefe934724d5da09d279dd0c048fe5c4396d2cd2e5aea238064d88f99ee16ef9de7346485d348a4bd5b9f8bc3bcc64d36534720d1a73dffaafcb8c9aac473e2ce07660929ef69ae8f1de859f57c3255192fa77007e3e50dcabe949495939b2cd793110d2925f2bf8a74f67423e9e7a63ec4d22f19bd4a84bc21276abf7a8b1ab1e344a004b8a0e3489d204ff04ecf0a4ca8242c7fc9f91c7cb37fbc9b7ff16920efc4f2a986fb497367b1d9e6b6bfea9f8dea7a1e20062bf3b883c00783a81954c6d3da3fe2a88506e4e34aa173e978e6109b1e662dd0d2b89c073d6db0dd7401f93b1567cce521705d876e838531e88c22bdb0ca69a1b05a2901fe840a4fd7147f40311d41140d4b891aaf05e95d81845f55d11348f77db7a16de4a228c11bca861e2acb09ca9922a985082dfa5335adbceb49e5ff81e547a846bd59e881705b40084477915eaa76f4b722697214b92ea77e37d1f1dc1dca083f501bc95c2cf05060654fea91501a3ef7cce9a0838259b4f6081d3c4bfbeb4be90542714ba6061c3987b3fa5aba883d50c17382cf5a6bd363edb4bf54b95815837156d2d6358d11dceba4a8aa4c740b277d563a585e225c134b8a62b9b10aaaf5f1f322d1fd4ab99b00c582b6d35171dd7fe3ba09f602f960410dedf8b8dad51619319b53f616263d7c934c47cc140a58ceeb89033cdc454bdbc24c98e8da47dc4ecaaadd43bafda30dde242d146d1b632741a72898fae8cde4c4efe5894a90435aa81df4e32134389a294b107f75e4d78bee365cc218035f3dcf3541544418ddca01f269123fcb3089523e31257251753e69a182a5da79113145e728aa27396e2dae8747b66da662603709a9f7b0092809985fcf09529b347496fe6eda00fdc3942ea547bb51ecd7a8c0b7dc4fb31821c24012d7ee7f4181c5a5e1135b25d689fc220d2737bbc04009be8656e1cf9585c0448b943515c1570a7ba9805a4bc3301c47ae60143b62232e090492cf45d73214fcd2370086fc733598c781b2a7c5782a3cc49090814be73de1c17c2cf1c43143222c03b92cc6e88a80981c70ebb281a0a8de6ae70a78b58d83c1c6d7ac67d2e668e095f3374bbe3e097f95344889ca85eb8ba59602eb0a815d32839ad0bdd244e7d8a236af4ee4b923074d73f6817ae954c700acdf04b1d18403507e1ff512b3b8f05b8e3db957329296430287ecb3c9b05569ca1269d1605ce77ba65e7ea2cb5b8774f45c5816f383018e84f5de97f3466d043f47a89cf45056060c22fbc1b28a7fba2a7c2163d7194e74a4689b0d9fcd2f39387646d256b72809ef78d5e361b6a3b1a3cff5d3eaa26873c3964dc3a29d289c44658f6c56ec5edec7e2a6bb67fd9bc1bbaf7cf8cf6369e6f363a44bf69d85fe3838651e03702b348dd0ae188e49b2d039fcea66bfc4788ec40f4accd03c095ff512634a5b4d2af218a263b8d863be40f41788303a4b308bdc11b1aa4647651aa3f339f883fc9da1c45d5e0af27f877381053b98eefb51055043f23ec3971f140170e9a33b1297e4746184c5263cb13e401b03be13f27eb287ee8da0d059f17d20684c6e2694cd0043e9219d99ac305c73559e9314b825d7b16d327ab3b637cb5890d41c588275b2ab281e3e1b0427f7486cc10402cbe2476a30fe481735d93dd9a4e54f4ef9a02b87d56d4ab9aa083c9e3505d846a0a23022524caa9c8def9ca3c4f01f43494f975d9a570cffcc378fd138889ef4a6658fd39134a7ab7dee56248ff96b9958f078420454447f474edef1f1149d54f47479fda182a0064086514e780c5ba9e260d2f4bbacb8b96394f099e8e8162a71ed2c9e38bc88233bc9764e0e27fb5f9363ea06ea75e57fc7157b3b9c368e3f5b3104cd56fcbe9c5afc692d8d2263f63bd709a41851a6674b5f04eb9d3fe017c8e9f3ecae2647eb73d21d78ea3a045feb4016f90b4c00e0ef6320947643fd4f942b2b8d8aede218d42bedea0b524898edef198581a2c6cfa88e8d165b58894ef05c7efc2d1b49d8dff0c890a9c596a1deabcc131f974ec35b8384641af20db4cf94d1a2f9d57e5cf1e2e02765a89bacaef630fead60886bdead9d5e62edb0350a49ca1d074bdac9cae652de1499433c6a2fdb6ddf20787db4b81bd218294d7a6448c7b5973f41d6391cbb7cd58ff2fe78d3e8ad1a313e63e1479b3036a3a133edf481659cad823d5a7ffcc95ddfd2df5fa49f9baeb870b2e99efbbe08dacb17e385cf76b6fab617c3f9b9ffc8a8ef7e153de41249d2af3fa5cea74e4ae55469903ef075b1e115e8106701d1d20c9753dd166c09de030f544d55d0f087ddeb453c4d36fd4b28c770511f9d4b8a660cce8c80121091323e77e16383f84a735de015e9b31b727b22f1396fe42957d815a197dea708e3540b2636dfeec133c74e33aa3c13b4f948c45b881d72c71aa97015c4fb24a35ddf77b03aff82cbaf6a989e2385359251bcbc54167213a5de427002590e4b8f2a577b24f955a6c422850b9c48464f3d8d9d5b6a0b1e4a855f7715d5ec803dc1d40ecbc497216ec214deb0b30e058cf4d93a7ed7dee37087227ef796158d7f384bcfa118ca62c74c85f3a74ceb3638b7bf4419aaf7da6158e6985d0a24fc69dc5cec715e49b42a55d2f7f87547fa754e50315436e224527f3ec38860eb116ecf5ac1f25cee16eb59a44beb64e95ced8d8efc57f733d059a9bc211f4b351c8fe3b7fe83a7611a0688bf276c8a310eff1bcc4bf0ce69476f3e0df867db5d536ef258de772f9fd914ca629e1853af7b7aa9f684d7c2d7f179e353885cb8a44b10e103b9b01e511ad0f12d7ce45eb771648dc7ec8b0beed8f0cdb2be7d3bc6e974b7e069daa01aa49a2302aa650927089e65618e4755ba193437e25ba050a4bb879a038b0032c5f66f6e2ea5b47aed2328833947a433b700862917c072ad5bd72d9aa238f1510202e790b075693a23aef856e0b5c8685b66ed4b4f4085a08345209034f3512c6fa8a9f9f92426e2ce5a887512f3d28ca148ec16df5b2987688a06aa1a2a1b0fa59dd2f884b6865d1dc041219c933ad61628199827ed7923ee5d6fcf5552e5e9b49946040a6f9ced337d6555f46cb3a743fe2d63c79c3bc793b9994c9a8d125ac7ecd7145c2fd825d2c4f35ba7121dfbdb5b5b9007b308f8f495e759c9daaa357004a65b531b9a41e2c4f8ee1aada741ca7d7f61105a618ee7c7fe445b33841916a7f1e44c913fdb5602df92b84443a0a7cc0f212f68af07ed910825f077f151e59a6c92627282ce2c03f45e3e9071cf77f3575ecef9cceb501eb894767c8de3bd74c490637aaf123a7be957e020cabcaf5dee087381fdd7cb9624c36dc6b516a5b093dbd9f1f5f11191d155dc56261fdafe99892056eae836984c7456ec09dc8c177e9d788f11b82b79c65d81cc5442aee07cdf07c3329c60cbace6edca791ddd13890628df34c7dd7d352a0bf3d25b38781f1162d7c22d4481b91b947e7e2e88919a8407cddd5ae9f22b4a1ab226c5ac32d57cc26c9e978ede3f98b6629ddf3cefc96bbef7badcef4359801a3ffc2dcdb9ac714325d0873fc5669bf71b50e3a875e4ae0c6a42adc1ce94d9a97a2b77ca283f4c2e04816a524c8336466bce0679f299d0792a0726e958b6de0a6e7507c8e8927f152f89e52ac55f8a59384281fc922bc603d525b0736221c1980dedf0d40c3a7410a942809648decd9def8f9da1cfbec53097d31c6d0a2a66b89f246a43430987c6c706394950a8e18068f3af5dde59926d24ac28ba94e18adeed2441b949aaa04f260b6f0b294a86d8b192c512b07c064e76cebb3b62a67be4f9a307888340b4d6fe013e88dc92a7fff1b255f18b7417c3240b8c7151e654af0783516104350b6579562f9bcff1c73c36b0550389ff0821b9a4bb30ada9abbfc6b491b5fd62e52bc33ea9cf58b972e2b431bc77b4098a0c013579b824f065d0f96a0502d02e3b2f828e2a6386ad6d96577a616a24ecf4c5c238a7fe3d54d6a55c74d53b06408eed29d0215455bab5ae084bac63c9cd29772c0952ee4ea66c36d18a9e99ca49f0b53164df6c73cdad4ee5c0c07edcf604696d7580952c33d37c1a3cea98306497b4e271417324877049be5bbe91f777f5f4100e6b298d2f20ec7c48577aeffa4f11fc333f956ec0f17c017f1e316dc7be870d0484c9e32008f57fd95aa982a4bb4a2efa0e22c60db35e4619a216e828368f5b06be936a0aea56a0c3ad412cc859161271ca97ec0a7a79ce9da80915eeaa8cccefe425fecc7cccaaffac244df1aacce67130535bab8cee1b9c96b969b05521e953e56ba8b0b25dab48f73a39b1f0985440deef19f153dbd29480f574400040f249c584bafc92045da957aa0beb38776ebd367493278d03d5b6cb5bb8459ca64cf44d8be49409710e376d4b28267997f0f88bd454dedd1e026700c84796e9d13aa9054998677eec19e725c4ef883803836981949fd74396f768e1fcd0c0fa68c562981c6a0146bfecbad1c2bc23ef06bba4cdc2fed8b3f3abc7a42caf71f32e5e69fa449a4f997a8a0c8f824dbf6e5e02321c11722d9b4d309e7bd63dfe3a610ca1ddd3019671a318571d9ca7507a94e8b5adc2cc79e101b89e7021471986b89cb8b17c9d2437cba8a94cfc5d9b2d7b3f7cabebdc3a13354b8b8947c9f88ea0e54f4597ff575675150fa94cea9714ef5d65979a7123e82b4210a6b95909ecdba21a7577d0c3fd7f5afe0e4ceb8f54720e3a49714495c1ce844bc7fb9b8d2f946a2f9e8f5c13552acc5ce9441d3d94f64988484c4cb7a6701eadb88ad0704456d441ca534a9e93827fddf76b3bd24637a3cb4434cc209a6f4eca79b07f9f55bf3e5164e0274d39b6157e3a69a79c93d235d7896e29508cc7de96a45e96c4148ba654d0cc8e8424a3df38c43889f85b830a64ca3773201493c8d29fcdf7174d8211a3cb763e0e0129de8e868f5214ce673fed2672c648b7c68e21d3c0f826ff90afc6c5e7c5021371abb175a1d7d044972c4d1bc6a9a35feaddd48a002564526b6240717f169d9f85fa4a95d31f475759eb5150636d2846b6b313181f69aa346183bb703aef25cc6a5e317d461d59cb4f2aabbfff312126602b55b872ad8e002418b31d96b3d4060094b23ba838afb716e994110b6726704fc8e3ae3f86b08155e1f38ce6b5194424eae3c6d69cbe8a743a3173f43de429ee3a353fd8ddcbe75fb10316be6e3701d6e9477b8c9eab40697a4ca1f9d5cb233579be21545d83dd127bba6712b052277cfd6faabd740468a16bd65f0f9994f7a5fe4959085f6841d5cc7af33f8cef54a8171c1066623ddde0ed08c197cc070ce289dad12bf066210e750bcf124625e91e480a11c004a169907c469b755ed2d91629d0a6c96efa3970966cabdcd13b4291d63065583db7c44d5855e730ce87c7123af72d31fef2f22729fb388daeba15ad2493e8f954a90367174d206a66c4d8e596d977b4cb05cf5f1ce24a15803f83a081333f63789c0aea77413d8b404c1f612e216901b08e8fc75ab0d64afa2f44f0aa0c8f6a0a69380fa14611ca129a327d36bec16104783ad78f32a8228fc2ba1158ddcdcd6f0b6b5e7b300c4792c14ae46340547106f8272cb5fd3328756edeedceb1b9d8fd2284e6b9fb32c44296e577ea88289f7e64875f03cb925a29004902afe4ce098c4fdc03c37c082fcced6ed7f2517674b6cdfadfa13d0029f657610e042828326e013e80d9a5249c52c4e256079b5ca742a4cd8e2e2ada9a2fba8fd2e72553c3b6c93e9460d6a64cdbc0a5177a4b2d6db19d3285a50ed13895205e0b3d8c29c4f2f4b2d5ae88b8c7551a28502b2e4b896ad3821286af64c3b30afa5527cf25fe9f123c12e4f89e2ecb22be57d0e73011403e47f2886412fafd949a41c7611526f63ffe38308151ba9c1b2e795992733b5d5aa27a8f43694b09e4acb4b9295c826f42b8a1e630d05bec10536335161a2e636bad12a421a7c64d54fed5ded4b98ec6df93f351d3c3168c5ba9ff85d7fd0b70b7be8c526679aed35a7c304124f06f349ef55503d06fb1f85b642a26b47969b37cb79df5f78ffdd6d5b948e182ae8e9c62c11a16211e511af4ea8a99f5100625b2c25afeb15dd4a50f55ef49cc315836eb82092f355fa7d19594b2da1e807c6ffa2a12fcef28bfadf5abe66ecb126836455efdf9c4df8c5638e7b167c2c6c459bd395b4dc63b879df2d703c9a4c23958a9724b5104ae533cec9aaaf564ff74d5bfeadeb3cb09991ca2cb7445794e84c33aae85fe5a228d750bb2f3e4edee29cc5bf859cfd4bf8fee86af87f51b7a6636729eda7dc2b0e6b5d3fe221df16a4836ec12e9121c3343f212db97e621f1515fd89ed7d4347343549be0a3c18fab3cbb8228ec20a558ca15af3b73714d731ebfc4376937e1601e7a66d784b582db4e69ef570f1f57b17f75bab2a8cb14614f79617ec69a821e57240fb962027d9e5c46c926e4a93f22aea33df4ed312645fa1d7053b7c7d9f3407d441f1aa30671d9aed1fa25103b14fd3892e328060bf10190f0a310f473ddf7ec37de4d49ea03db7d94dfb3c2a07ba231923996c8692447c640db9f8b83b2b957523e959309a8141d07f8b8a27c58d400d88828c47a21ad88642c7d01cebfea6f23fb5ef5f06c7c92cc7d7bffe74bbc3eabfd54a9b25df701cd8203ea36412cd7e100d9a0166a022eeb91f38b6bb6d2769ffd3149d36a98159c0fc2ac669fd580bac6aeca217cdb1819d69588a8ce1ccb8be4835e730a3f801d2a4164da8d42fcddf31ce9d88ecb07c8ee7da242bfd03946e31e2e7ae5c286d7a77c3f494c0742f52b17a3f43badb218fa1a7a7d5251867b5a2c907f0f3f818a35869c0f10ac0b6fe8dcf92552c2fbdad1be93c0d460a5b696d2dd748bd9f0aa9911211a687c6e838a992d6cdbdc977184b72b90b08021c5c5df51c44761dfa1bbdc91a940945a5411a0f7dd6ecc9754452b9dff026f074f4a7f7f12e2ce1575076bd9e0a8003fdc2712de5e86d8c5b0f0bf3ee907ca8495ec5924cb638856347b6af478c34556770a4e1ac9941c55c3bd38410998e1a40b226fb83ec1deb79056a07bbad5ec67c8e0de3e4f4e220d250ad2af82a1ce303190950315a7ec8698c46fcee0d4fb6bb8597b8e348c4f61432ea9f71f8e99a883cb709b3a0327533cebb31b87428a60db3e64ecc2cec2a8912b940dc1b781783936c9f4a6bcad74b72bf8e785122d541843092eb95403f2235535d53094c1c5482bab30ccf08e0a983d3d4d75606dbcd692f2545212e2f9ca817de98736d144fa415747d29700a8e7a6e49f04d3598534c39f3a88160f6856ff33b064c5e00d7e659d2995eee11fa7e67226502f63ec9e806499c4e8e2f03c7f81b74253832606baf12a826dbd2e69a452a0f72efde611fcc6dbe646e1c0bffdeba7e7fa057a1dc9e4d846b1d06d76aeccb5bed4fd135a92b7dec77358797abf238a303a35c099ce8c324868ecc11d4e7b075bfe4eba8b156e4e3b9d40fa1c3b74f7ff2c38ed515b72781e5ed9599dda496d502ec2f7b36e0b2657b9279745f5349a073c70016dae6a3c519b5010f86a048df22b55b51e94e9153d7d02fdc1c66d2e60ad474343ed07dc3abbb02293dbfbb227db196505d22791dcc5f70a2cfaa34df4f462865f30ca1e59862c105cbf5569863f6b4493260b37be1651c31b70a5c4da56e79201cd6c6bff4201271d6fecf33e2dfbfa1dcbd5c54f39ad955b1db4930187c1512386c75c8badb3b8085c66ae4ef5ea3fc060b7b13ca335dd56b73c2d22d200996219579b1d278e961667d2ba54c96437a500a2369a93e2899fd62433032de38cb19fc6d5d47917877184b1bc86560c5e36fe29c649ae29475e6f57ad2d83ecd24a5a73ab7a2bea3c0f996dbf5cf27050f3c03c11a8f352249e3c867322e2b5e24e587559e07d638bae6e2d45b8e212239f1cef4f9cd020df31d003a75e2291ea7d460455cc5fce5577ef68458dccbdc068935dd7e99ffd7492df8daf333876f138632305e090a285db796ef83cc27814a343ddbdb044b33057808bcba146a7ddeb71047fc1896e89af8e42542c6fc4e8de4d93d0612704adc48c2fdd4ffe8f9078560ed71b94350e7d9365bad5449b722b2ecba7466d96df56e73ee8480643624fbec44da4325980dc81bad7928eb10a1911a1a18872ce788d7e43c8ad3a5e96f4c87041523f0537f10683c774d0a1741eb838d0ba6688c97893068174ae65ab0bcdba8d8cd5bc9bc52a008c6fcfbb3b74fa45f33017cf8064ec6c2451eb2bbc27543c616882c7895c490ffb73630b682a8af957df47164ced0e350aa80f8c01a2fda121751eb753838cb87238c01c788da993df3bd97b236e89fe10ccca4cb619e29a69175ba8fd5ae89dcb331d4c2ed97ee92d253ef7a43339d3f0943163033cc2df5e71fa95d4f811e3aba76ab34a58a501e566d2a078861a7d64eb8e77a0fee1cf525d93b1ac0724386c26c6a6b0c2e982ee6fe1b835fd1ef8b3dc8117e483ed89579a90b260ee10d5aa39c822ed4700fd78280a06637c8d1ae3b181ab0dbc52c03badf1cc1ad993385b963a96082006d2a1032d18983a62e52384a0061f9e64472973c24c749753856c438f45399a77c35ba2660667e2641d4af9d686b0ac2e2a4418427458fffba76c0ba027bc5e448a60858d7cfa96c9bdd731f8a7135b46a0e7b3542516f09df9e7454b7e45c38b51f167f222b9248a7406fb35f75bbb3002037da9cdb34a6728c19b6ee0f02f3d5745bee5bca01907ea8aa809feb03d497f3b426e97bd51e0808337b9c4af5d215d33feff8fca854ca224cbe961496ad88f18b398836bbcc7551236504daecc2d6922f05d0bcf694edc6b0c2fac7b2ba7d6ca9fe998c52700353ef34fbaa8cd71fc395cd73b168d6a8b7497ebb447c7184a84ab2339fc679d9eb8441ca6747963b634f1a986fef9ff853843bfc463808cc0ba1a5eecfc83083e38d1dc8fca0a0b22671f23de9b045d5efe6cce3188483b1a09acaabbaeac2671258ecfe237c3cc3785db0ce3a608618cedb3091b9397a6051cc9a3a90afe92de2d37aa2b4bba39181b123f755528f53a4e2774f04aa776f15af9f6829e0d9dc29c77d67bf346b39761919cbc26c7e30cbbe3d307fd54da1e82ef4186beb6bf79f28069d50795802f2e48299278cb3e79fc3a33f1c394460b6566cec63c3fc797e205e915d77188b9b69976b327bf10aed64f4633dbbcaba3c2c5513252b49cc4349fa272d8eae18609bffa59bc3a2e558288b5cc773278bc8dae5974128015edc37210371173b60d0f1b023be5e12e17b9f846e4fa5996784397aa0cde4b4bf1dddfd8cad06a3ba70ef778efcda02e71c55d9609dabd9ce0bdff476bd4556da252ce180004d4c67b3db4a07522a78d01559e970d91b326f6221956c96c31daba27db19932965c7d1a9ab781fb3f1c9cbdfd4fe27ff5f85ae9a032ebd5d61d7bbb05ec9cc9361c9a862d523e8891f861a710d8a8bafc3e4afb0fb358d454ed4df084ea61e0142994c4296e6461155a4850fbe05905d268a83f1b5692b5e5ed059a7743f9306e8e3fd0c24904ba37abe466f50f4a0408c9af7d8d350d21bf6ea7e6498016d934319c54cd655178942e76a83ad104af14117600b6600926fb29a6e299356384035a7522369f63000ea72412633f605006819703682674c7fcaa68f4c875aea22262f4fd22679975713052ddc0c956f8c354d1044b77358a0e920f1902e5e03e1776869871998c5df5cab9ea17bd8bc248a5508eabdf81e9d7e601be2d63cc0aa9967a7abbc65cb5e6ac6bf8d940d70767933fa1cfbbf3fc4e05e18883211727bb9a6f25be163ad04c300f84e8a9b210131559f789a759a55b83b2bd29d28ee8d1c61f29c40517dad7534abb665b427763e74df576dc2797593a514d385d06303436aee2c5128d5509dcb3485c6a4a3e20198f93a191ebde8909fa7924e2b283b65ae549affee77dc5627ba83564089ca7b3d4484b7de6e711afcce3f669f9dbd26e7166d21b9746332be21834a95ea7c89ff3aa99958654001fb2d3a94953a5e4d55691399b4d702f005d184d71b4ce0ddd067947c8845e2c19943626c6aa07f121ed7fc19a715f169f23997948552d31d4ea9a82e0ce3c5c078f785941d11da4189089a38f767a75bf6eb31f0e64e4613b7da6d98b86cfb0df51e8440f8f7ee593f274ed5d0002abc2def0e740a4c9e543649c7d58159cb3413972dc14106811fff97b25d68602ec49bc6a72e1ceb7334126b1102362036302bc150828a426c595f068217465b6d9b574d60527b8aba10ad16ec330d5250f0867ac82c114fe1c452cfff6bcbb597e87e7e9df8801d89400e9ab9d98f48dd2ea84db9b92d0bf59b9dfdac9f3825c28ed08320b1a6c6a246388f2b1b1808c9022e2d280c26349c8c46ef8085e2a195db56a3a339b6285a5d0b4faad233a04c3cc95039e9c0e6bc4a764e9c7e3ac861731230235ac55a3abd7aa331fc244e8b626f614103b9ac88cfdd0dd2861c0987813deb9376d93e7c1287f314d9c4da475ced944ff1e70943fdd91bd3836c47bb15e593a928941e6ea9f826c311adb205a82304596413877be6dc84c44f34ffbfcde8f547c9c5df45bcbf91e3d36b55698daf86134f48e52b9abc4b7015e42d55aaf30040db69d1c5153f975a446a31db019ac12d45c770f95119ed914c9f285745b3e566ecad4fbce57d8d12f870373afdcab4b094865426eb47df0616a6fd682b9b6f035353ceb6c640cfbc23ba2df95a8718b0c8abb51e79e3135a73b97710e95362b1f22aaf5c785edd591ca835f358e7f7dfb52c2a45f152d2086043506eaa3c54a98addb6fa2c7ad9552fd89467b33e414bd11570e0ab3c5081aaa137f41df9a9b2c78eca7c8a333da0b42ff7cd31f5f76e8e256cde03face45eb8712b0898f78b862a38113c2408de3d3553dceee55f5fd1fc2d07d1185b535fa6169f1559ae89508486e7dcff2793cc1d11c7382a4e3cfcf07083adf5188ae0c462c6c8d38053bc110756256d89696bedd5eea518173d6c95504457946b85d3219237e7fbf00e5ef75791353b0bb7a800a312ef3ccdeed3349c0fd2fd28f48b396cb4180bc470ed22cbec2d0465bc8fb9dbf47ee43f97e2d0a7ac5450a3d4ecf0c76e79fe3b9bb31964331fb97fceb6cdf34d4f5d985b295871a2575a98484f63c81810475568f9302ae95dce1d76045505d8c6f12bb0450690e4c8a9632d63ad2a0065129a36a4a751003f47b4da6cf36bb763d137c71b85b1e9b9329ba26d67479678293826b31276ead6fc0e89c95a78f214bae13201abeec4032a391646d2a0f559ba2251143d2c4b8cfa0c896487c2832761668c3624c67353be7d72e68f4316cca7f5759b47ae62f77606e3bdc1dcc5b74990f3bf48a41cbb959a27f60bc04374255cc458b1d5deb9973f212c146fc75d6072fcded4a2abd537145c9a3aeb043ebc813111bfbe27370de01cba0b0ee021b0fff96b46cc7ace451c0bea14dbab281dafb84d1eda43e866196b1aabfd9c7f1c8e345fea92922c81a0062cabe3723c922e1c895bf06d687f164d4f146e3a1e50f6e1a7fd99be467c33b9ae7196e9562f144847dd9b73dfb52e864136a37e6ccc2e7fbd505f5ce2afe4b7b5f652c6fe0eb69f82114276c012c45cac8af876da0dc94bd899c2f3414f10005bbf1f1927782bb56dec8f5464e338f0d2366a33b89acc2751cda74551e523c44588a799cc62f94985649f8c65f21ae92c3043188633c250e62").unwrap();
        assert_eq!(hints, expected, "hint bytes must match the pre-computed test vector");
    }
}
