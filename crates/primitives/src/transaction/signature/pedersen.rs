//! Blinded Pedersen vector commitments over BN254 G1.
//!
//! Given key hashes `h_0, ..., h_{m-1}` and a random blinding factor `rho`:
//!
//! ```text
//! C = h_0 * G_1 + h_1 * G_2 + ... + h_{m-1} * G_m + rho * B
//! ```
//!
//! Generator points `G_i` and `B` are derived deterministically via a
//! hash-and-try method with no trusted setup. Each generator has an unknown
//! discrete log relative to every other, ensuring the commitment is both
//! perfectly hiding and computationally binding under the discrete log
//! assumption on BN254.

use alloy_primitives::{B256, U256, keccak256};
use alloy_rlp::{BufMut, Decodable, Encodable, length_of_length};
use ark_bn254::{Fq, Fr, G1Affine, G1Projective};
use ark_ec::CurveGroup;
use ark_ff::{Field, PrimeField};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use ark_std::UniformRand;
use once_cell::sync::Lazy;
use std::ops::Mul;

use super::keychain::{KeyCommitment, MAX_AUTHORIZED_KEYS};

// ---- Errors ----------------------------------------------------------------

/// Errors arising from Pedersen commitment operations.
#[derive(thiserror::Error, Debug)]
pub enum PedersenError {
    #[error("too many key hashes: got {got}, max {MAX_AUTHORIZED_KEYS}")]
    TooManyKeys { got: usize },

    #[error("point decompression failed")]
    DecompressionFailed,

    #[error("serialization error: {0}")]
    Serialization(#[from] ark_serialize::SerializationError),
}

// ---- Generator derivation --------------------------------------------------

/// The BN254 curve equation constant: y^2 = x^3 + 3.
const BN254_B: u64 = 3;

/// Derive a G1 point whose discrete log relative to the standard generator
/// (and to every other derived point) is unknown.
///
/// Uses a hash-and-try approach: hash the tag (with a counter) to an
/// x-coordinate in Fq, check whether a valid y exists on BN254, and verify
/// subgroup membership.
fn derive_generator(tag: &[u8]) -> G1Projective {
    let mut counter = 0u64;
    loop {
        let mut input = Vec::with_capacity(tag.len() + 8);
        input.extend_from_slice(tag);
        input.extend_from_slice(&counter.to_le_bytes());
        let hash = keccak256(&input);

        // Interpret the 32-byte hash as a base field element.
        let x = Fq::from_be_bytes_mod_order(hash.as_ref());

        // y^2 = x^3 + 3
        let x_cubed = x * x * x;
        let rhs = x_cubed + Fq::from(BN254_B);

        if let Some(y) = rhs.sqrt() {
            // Choose the lexicographically smaller y to ensure determinism.
            let y_neg = -y;
            let y_final = if y < y_neg { y } else { y_neg };

            let affine = G1Affine::new_unchecked(x, y_final);
            if affine.is_on_curve() && affine.is_in_correct_subgroup_assuming_on_curve() {
                return affine.into();
            }
        }

        counter += 1;
    }
}

// ---- PedersenGenerators ----------------------------------------------------

/// The set of BN254 G1 generator points for vector Pedersen commitments.
///
/// Contains `MAX_AUTHORIZED_KEYS` value generators `G_1, ..., G_n` and one
/// blinding generator `B`. All points are derived deterministically from
/// domain-separated tags with no trusted setup.
#[derive(Debug, Clone)]
pub struct PedersenGenerators {
    /// `G_1, ..., G_n` — one generator per key hash slot.
    pub g: Vec<G1Projective>,
    /// `B` — the blinding generator.
    pub b: G1Projective,
}

impl PedersenGenerators {
    /// Derive the full generator set from fixed domain-separation tags.
    pub fn new() -> Self {
        let g = (0..MAX_AUTHORIZED_KEYS)
            .map(|i| {
                let tag = format!("WorldChain.Pedersen.G{i}");
                derive_generator(tag.as_bytes())
            })
            .collect();

        let b = derive_generator(b"WorldChain.Pedersen.B");

        Self { g, b }
    }
}

impl Default for PedersenGenerators {
    fn default() -> Self {
        Self::new()
    }
}

/// Lazily-initialized singleton for the canonical generator set.
pub static GENERATORS: Lazy<PedersenGenerators> = Lazy::new(PedersenGenerators::new);

// ---- Scalar conversion -----------------------------------------------------

/// Convert an alloy [`U256`] to an ark-bn254 scalar field element [`Fr`].
///
/// The value is reduced modulo the BN254 scalar field order.
pub fn u256_to_fr(v: U256) -> Fr {
    let bytes = v.to_be_bytes::<32>();
    Fr::from_be_bytes_mod_order(&bytes)
}

// ---- PedersenCommitment ----------------------------------------------------

/// The byte length of a compressed BN254 G1 point.
pub const COMPRESSED_G1_LEN: usize = 32;

/// A blinded Pedersen vector commitment -- a single BN254 G1 point.
///
/// ```text
/// C = sum_{i=0}^{m-1} h_i * G_{i+1} + rho * B
/// ```
///
/// The commitment is perfectly hiding (the blinding factor `rho` is uniformly
/// random) and computationally binding under the discrete-log assumption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PedersenCommitment {
    /// The commitment point in projective coordinates.
    pub point: G1Projective,
}

impl PedersenCommitment {
    /// Compute the blinded Pedersen vector commitment from key hashes and a
    /// blinding factor.
    ///
    /// # Errors
    ///
    /// Returns [`PedersenError::TooManyKeys`] if `key_hashes.len()` exceeds
    /// [`MAX_AUTHORIZED_KEYS`].
    pub fn commit(
        key_hashes: &[U256],
        blinding: U256,
        generators: &PedersenGenerators,
    ) -> Result<Self, PedersenError> {
        if key_hashes.len() > MAX_AUTHORIZED_KEYS {
            return Err(PedersenError::TooManyKeys {
                got: key_hashes.len(),
            });
        }

        let mut acc = G1Projective::default(); // point at infinity

        for (h, g) in key_hashes.iter().zip(generators.g.iter()) {
            let scalar = u256_to_fr(*h);
            acc += g.mul(scalar);
        }

        let rho = u256_to_fr(blinding);
        acc += generators.b.mul(rho);

        Ok(Self { point: acc })
    }

    /// Serialize the commitment point to compressed form (32 bytes).
    pub fn to_compressed(&self) -> [u8; COMPRESSED_G1_LEN] {
        let affine = self.point.into_affine();
        let mut buf = Vec::with_capacity(COMPRESSED_G1_LEN);
        affine
            .serialize_compressed(&mut buf)
            .expect("compressed G1 serialization should not fail");

        let mut out = [0u8; COMPRESSED_G1_LEN];
        out.copy_from_slice(&buf);
        out
    }

    /// Deserialize from a compressed BN254 G1 point (32 bytes).
    pub fn from_compressed(bytes: &[u8; COMPRESSED_G1_LEN]) -> Result<Self, PedersenError> {
        let affine = G1Affine::deserialize_compressed(&bytes[..])
            .map_err(|_| PedersenError::DecompressionFailed)?;
        Ok(Self {
            point: affine.into(),
        })
    }

    /// Derive the [`KeyCommitment`] by hashing the compressed point with keccak256.
    ///
    /// This is the value embedded in the account address:
    /// `addr = bytes20(keccak256(nu_a || commitment_bytes))`
    pub fn to_key_commitment(&self) -> KeyCommitment {
        let compressed = self.to_compressed();
        KeyCommitment(B256::from(compressed))
    }
}

// ---- RLP encoding / decoding -----------------------------------------------

impl Encodable for PedersenCommitment {
    fn encode(&self, out: &mut dyn BufMut) {
        let compressed = self.to_compressed();
        compressed.as_ref().encode(out);
    }

    fn length(&self) -> usize {
        // RLP encoding of a 32-byte byte string: 1 byte header + 32 bytes
        let inner_len = COMPRESSED_G1_LEN;
        inner_len + length_of_length(inner_len)
    }
}

impl Decodable for PedersenCommitment {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let bytes: alloy_primitives::FixedBytes<COMPRESSED_G1_LEN> = Decodable::decode(buf)?;
        Self::from_compressed(&bytes.0)
            .map_err(|_| alloy_rlp::Error::Custom("invalid compressed G1 point"))
    }
}

// ---- serde -----------------------------------------------------------------

#[cfg(feature = "serde")]
impl serde::Serialize for PedersenCommitment {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let compressed = self.to_compressed();
        let hex_str = alloy_primitives::hex::encode_prefixed(compressed);
        serializer.serialize_str(&hex_str)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for PedersenCommitment {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        let bytes = alloy_primitives::hex::decode(&s).map_err(serde::de::Error::custom)?;
        if bytes.len() != COMPRESSED_G1_LEN {
            return Err(serde::de::Error::custom(format!(
                "expected {COMPRESSED_G1_LEN} bytes, got {}",
                bytes.len()
            )));
        }
        let arr: [u8; COMPRESSED_G1_LEN] = bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("invalid length"))?;
        Self::from_compressed(&arr).map_err(serde::de::Error::custom)
    }
}

// ---- Membership proof (Sigma / Schnorr-style) --------------------------------

/// A zero-knowledge proof that component `j` of the committed vector equals a
/// claimed value `v`, without revealing any other component.
///
/// The proof is a Sigma protocol (Schnorr-like) made non-interactive with
/// Fiat-Shamir:
///
/// ```text
/// Prover knows full opening (h_0, ..., h_{m-1}, rho) of C.
///
/// 1. Sample random r_i for each i != j, and random s.
/// 2. Compute R = sum_{i != j} r_i * G_{i+1} + s * B.
/// 3. Derive challenge c = keccak256(compress(C) || j || v_bytes || compress(R)) mod r.
/// 4. Compute responses: z_i = r_i + c * h_i for i != j, z_rho = s + c * rho.
///
/// Verifier checks:
///   sum_{i != j} z_i * G_{i+1} + z_rho * B  ==  R + c * (C - v * G_{j+1})
/// ```
///
/// Proof size is O(m) field elements where m is the number of key slots used
/// in the commitment. For m <= 20 this is at most ~672 bytes (20 * 32 + 32),
/// which is acceptable for on-chain submission.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MembershipProof {
    /// The key-slot index being proven (0-based).
    pub key_index: u8,
    /// Compressed R point (the Sigma-protocol commitment).
    pub r_point: [u8; COMPRESSED_G1_LEN],
    /// Response scalars z_i for each key slot i != key_index.
    ///
    /// The vector has length `num_keys - 1`. Slot ordering: indices
    /// `0, 1, ..., key_index-1, key_index+1, ..., num_keys-1`.
    pub responses: Vec<U256>,
    /// Blinding response z_rho = s + c * rho.
    pub blinding_response: U256,
}

impl MembershipProof {
    /// Create a membership proof that `key_hashes[key_index] == claimed_value`
    /// within the commitment `C = commit(key_hashes, blinding, generators)`.
    ///
    /// # Arguments
    ///
    /// * `key_hashes`  - The full vector of key-hash scalars (field elements).
    /// * `blinding`    - The blinding factor rho used to form C.
    /// * `key_index`   - Index j of the component to prove membership for.
    /// * `generators`  - The Pedersen generator set.
    /// * `rng`         - A cryptographically-secure random number generator.
    ///
    /// # Errors
    ///
    /// Returns [`PedersenError::TooManyKeys`] if the key set exceeds the
    /// maximum, or panics if `key_index >= key_hashes.len()`.
    pub fn prove<R: ark_std::rand::Rng>(
        key_hashes: &[U256],
        blinding: U256,
        key_index: usize,
        generators: &PedersenGenerators,
        rng: &mut R,
    ) -> Result<Self, PedersenError> {
        let m = key_hashes.len();
        if m > MAX_AUTHORIZED_KEYS {
            return Err(PedersenError::TooManyKeys { got: m });
        }
        assert!(key_index < m, "key_index {key_index} out of range (m={m})");

        // Recompute the commitment C.
        let commitment = PedersenCommitment::commit(key_hashes, blinding, generators)?;

        // Step 1: sample random masking scalars.
        let mut nonce_scalars: Vec<Fr> = Vec::with_capacity(m - 1);
        for _ in 0..(m - 1) {
            nonce_scalars.push(Fr::rand(rng));
        }
        let s = Fr::rand(rng); // blinding nonce

        // Step 2: compute R = sum_{i != j} r_i * G_{i+1} + s * B.
        let mut r_acc = G1Projective::default(); // identity
        let mut nonce_idx = 0usize;
        for i in 0..m {
            if i == key_index {
                continue;
            }
            r_acc += generators.g[i].mul(nonce_scalars[nonce_idx]);
            nonce_idx += 1;
        }
        r_acc += generators.b.mul(s);

        // Step 3: Fiat-Shamir challenge.
        let c_compressed = commitment.to_compressed();
        let r_affine = r_acc.into_affine();
        let mut r_compressed_buf = Vec::with_capacity(COMPRESSED_G1_LEN);
        r_affine
            .serialize_compressed(&mut r_compressed_buf)
            .expect("G1 compression should not fail");
        let mut r_compressed = [0u8; COMPRESSED_G1_LEN];
        r_compressed.copy_from_slice(&r_compressed_buf);

        let challenge = fiat_shamir_challenge(
            &c_compressed,
            key_index as u8,
            &key_hashes[key_index],
            &r_compressed,
        );

        // Step 4: compute responses.
        let rho = u256_to_fr(blinding);
        let mut responses = Vec::with_capacity(m - 1);
        let mut nonce_idx = 0usize;
        for i in 0..m {
            if i == key_index {
                continue;
            }
            let h_i = u256_to_fr(key_hashes[i]);
            // z_i = r_i + c * h_i
            let z_i = nonce_scalars[nonce_idx] + challenge * h_i;
            responses.push(fr_to_u256(z_i));
            nonce_idx += 1;
        }

        // z_rho = s + c * rho
        let z_rho = s + challenge * rho;

        Ok(Self {
            key_index: key_index as u8,
            r_point: r_compressed,
            responses,
            blinding_response: fr_to_u256(z_rho),
        })
    }

    /// Verify that key slot `self.key_index` of `commitment` equals
    /// `claimed_value`.
    ///
    /// # Arguments
    ///
    /// * `commitment`    - The Pedersen vector commitment C.
    /// * `claimed_value` - The value v claimed to be at slot j.
    /// * `num_keys`      - The number of key slots used when forming C.
    /// * `generators`    - The Pedersen generator set.
    pub fn verify(
        &self,
        commitment: &PedersenCommitment,
        claimed_value: U256,
        num_keys: usize,
        generators: &PedersenGenerators,
    ) -> bool {
        let j = self.key_index as usize;

        // Basic bounds checks.
        if num_keys > MAX_AUTHORIZED_KEYS || j >= num_keys {
            return false;
        }
        if self.responses.len() != num_keys - 1 {
            return false;
        }

        // Decompress R.
        let r_point = match G1Affine::deserialize_compressed(&self.r_point[..]) {
            Ok(p) => G1Projective::from(p),
            Err(_) => return false,
        };

        // Recompute Fiat-Shamir challenge.
        let c_compressed = commitment.to_compressed();
        let challenge =
            fiat_shamir_challenge(&c_compressed, self.key_index, &claimed_value, &self.r_point);

        // LHS: sum_{i != j} z_i * G_{i+1} + z_rho * B
        let mut lhs = G1Projective::default();
        let mut resp_idx = 0usize;
        for i in 0..num_keys {
            if i == j {
                continue;
            }
            let z_i = u256_to_fr(self.responses[resp_idx]);
            lhs += generators.g[i].mul(z_i);
            resp_idx += 1;
        }
        let z_rho = u256_to_fr(self.blinding_response);
        lhs += generators.b.mul(z_rho);

        // RHS: R + c * (C - v * G_{j+1})
        let v_fr = u256_to_fr(claimed_value);
        let c_minus_v_gj = commitment.point - generators.g[j].mul(v_fr);
        let rhs = r_point + c_minus_v_gj.mul(challenge);

        lhs == rhs
    }
}

/// Derive the Fiat-Shamir challenge scalar from the transcript.
///
/// `c = keccak256(compress(C) || key_index || value_be_bytes || compress(R)) mod r`
fn fiat_shamir_challenge(
    commitment_compressed: &[u8; COMPRESSED_G1_LEN],
    key_index: u8,
    claimed_value: &U256,
    r_compressed: &[u8; COMPRESSED_G1_LEN],
) -> Fr {
    let mut transcript = Vec::with_capacity(COMPRESSED_G1_LEN + 1 + 32 + COMPRESSED_G1_LEN);
    transcript.extend_from_slice(commitment_compressed);
    transcript.push(key_index);
    transcript.extend_from_slice(&claimed_value.to_be_bytes::<32>());
    transcript.extend_from_slice(r_compressed);
    let hash = keccak256(&transcript);
    Fr::from_be_bytes_mod_order(hash.as_ref())
}

/// Convert an ark-bn254 scalar field element [`Fr`] to an alloy [`U256`].
///
/// Uses big-endian canonical serialization. The result is always in
/// `[0, r)` so fits in 256 bits.
pub fn fr_to_u256(f: Fr) -> U256 {
    let bigint = f.into_bigint();
    let mut be_bytes = [0u8; 32];
    // ark PrimeField BigInt is little-endian limbs (u64).
    // We need big-endian bytes.
    for (i, limb) in bigint.0.iter().enumerate() {
        let start = 32 - (i + 1) * 8;
        be_bytes[start..start + 8].copy_from_slice(&limb.to_be_bytes());
    }
    U256::from_be_bytes(be_bytes)
}

// ---- MembershipProof RLP encoding / decoding ---------------------------------

impl Encodable for MembershipProof {
    fn encode(&self, out: &mut dyn BufMut) {
        // Encode as an RLP list: [key_index, r_point, [responses...], blinding_response]
        let header = alloy_rlp::Header {
            list: true,
            payload_length: self.payload_length(),
        };
        header.encode(out);
        self.key_index.encode(out);
        self.r_point.as_ref().encode(out);
        // Encode responses as a nested RLP list.
        let resp_header = alloy_rlp::Header {
            list: true,
            payload_length: self.responses_payload_length(),
        };
        resp_header.encode(out);
        for r in &self.responses {
            r.encode(out);
        }
        self.blinding_response.encode(out);
    }

    fn length(&self) -> usize {
        let payload = self.payload_length();
        payload + length_of_length(payload)
    }
}

impl MembershipProof {
    /// RLP payload length (contents of the outer list, excluding the list header).
    fn payload_length(&self) -> usize {
        let key_index_len = self.key_index.length();
        let r_point_len = {
            let inner = COMPRESSED_G1_LEN;
            inner + length_of_length(inner)
        };
        let responses_list_len = {
            let inner = self.responses_payload_length();
            inner + length_of_length(inner)
        };
        let blinding_len = self.blinding_response.length();
        key_index_len + r_point_len + responses_list_len + blinding_len
    }

    /// Payload length of the inner responses list.
    fn responses_payload_length(&self) -> usize {
        self.responses.iter().map(|r| r.length()).sum()
    }
}

impl Decodable for MembershipProof {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let header = alloy_rlp::Header::decode(buf)?;
        if !header.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let remaining_before = buf.len();

        let key_index = u8::decode(buf)?;
        let r_bytes: alloy_primitives::FixedBytes<COMPRESSED_G1_LEN> = Decodable::decode(buf)?;

        // Decode responses list.
        let resp_header = alloy_rlp::Header::decode(buf)?;
        if !resp_header.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let resp_remaining_before = buf.len();
        let mut responses = Vec::new();
        while resp_remaining_before - buf.len() < resp_header.payload_length {
            responses.push(U256::decode(buf)?);
        }

        let blinding_response = U256::decode(buf)?;

        // Verify we consumed exactly the right number of bytes.
        let consumed = remaining_before - buf.len();
        if consumed != header.payload_length {
            return Err(alloy_rlp::Error::ListLengthMismatch {
                expected: header.payload_length,
                got: consumed,
            });
        }

        Ok(Self {
            key_index,
            r_point: r_bytes.0,
            responses,
            blinding_response,
        })
    }
}

#[cfg(feature = "arbitrary")]
impl<'a> arbitrary::Arbitrary<'a> for MembershipProof {
    fn arbitrary(u: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        let key_index = u.int_in_range(0u8..=19)?;
        let r_point = <[u8; COMPRESSED_G1_LEN]>::arbitrary(u)?;
        let num_responses = u.int_in_range(0usize..=19)?;
        let mut responses = Vec::with_capacity(num_responses);
        for _ in 0..num_responses {
            responses.push(U256::from_be_bytes(<[u8; 32]>::arbitrary(u)?));
        }
        let blinding_response = U256::from_be_bytes(<[u8; 32]>::arbitrary(u)?);
        Ok(Self {
            key_index,
            r_point,
            responses,
            blinding_response,
        })
    }
}

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Generators must be deterministic across calls.
    #[test]
    fn generators_are_deterministic() {
        let g1 = PedersenGenerators::new();
        let g2 = PedersenGenerators::new();
        assert_eq!(g1.g.len(), g2.g.len());
        for (a, b) in g1.g.iter().zip(g2.g.iter()) {
            assert_eq!(a, b);
        }
        assert_eq!(g1.b, g2.b);
    }

    /// All generators should be distinct (no collisions among G_i and B).
    #[test]
    fn generators_are_distinct() {
        let gens = &*GENERATORS;
        let mut all_points: Vec<G1Projective> = gens.g.clone();
        all_points.push(gens.b);

        for i in 0..all_points.len() {
            for j in (i + 1)..all_points.len() {
                assert_ne!(
                    all_points[i], all_points[j],
                    "generators {i} and {j} collide"
                );
            }
        }
    }

    /// The same inputs must always produce the same commitment.
    #[test]
    fn commitment_is_deterministic() {
        let hashes = vec![U256::from(42), U256::from(99)];
        let blinding = U256::from(7);
        let c1 = PedersenCommitment::commit(&hashes, blinding, &GENERATORS).expect("commit failed");
        let c2 = PedersenCommitment::commit(&hashes, blinding, &GENERATORS).expect("commit failed");
        assert_eq!(c1, c2);
    }

    /// Different blinding factors must produce different commitments (hiding).
    #[test]
    fn different_blinding_produces_different_commitment() {
        let hashes = vec![U256::from(42)];
        let c1 =
            PedersenCommitment::commit(&hashes, U256::from(1), &GENERATORS).expect("commit failed");
        let c2 =
            PedersenCommitment::commit(&hashes, U256::from(2), &GENERATORS).expect("commit failed");
        assert_ne!(c1, c2);
    }

    /// Different key sets must produce different commitments (binding).
    #[test]
    fn different_keys_produce_different_commitment() {
        let blinding = U256::from(7);
        let c1 = PedersenCommitment::commit(&[U256::from(1)], blinding, &GENERATORS)
            .expect("commit failed");
        let c2 = PedersenCommitment::commit(&[U256::from(2)], blinding, &GENERATORS)
            .expect("commit failed");
        assert_ne!(c1, c2);
    }

    /// Compressed representation must roundtrip faithfully.
    #[test]
    fn compressed_roundtrip() {
        let hashes = vec![U256::from(42), U256::from(99), U256::from(7)];
        let blinding = U256::from(12345);
        let original =
            PedersenCommitment::commit(&hashes, blinding, &GENERATORS).expect("commit failed");

        let compressed = original.to_compressed();
        let recovered =
            PedersenCommitment::from_compressed(&compressed).expect("decompression failed");

        assert_eq!(original, recovered);
    }

    /// RLP encoding must roundtrip.
    #[test]
    fn rlp_roundtrip() {
        let hashes = vec![U256::from(100)];
        let commitment =
            PedersenCommitment::commit(&hashes, U256::from(9), &GENERATORS).expect("commit failed");

        let mut buf = Vec::new();
        commitment.encode(&mut buf);
        let decoded =
            PedersenCommitment::decode(&mut buf.as_slice()).expect("RLP decode should succeed");

        assert_eq!(commitment, decoded);
    }

    /// `to_key_commitment` must be deterministic and match the compressed bytes.
    #[test]
    fn key_commitment_from_pedersen() {
        let hashes = vec![U256::from(1), U256::from(2)];
        let blinding = U256::from(999);
        let pc = PedersenCommitment::commit(&hashes, blinding, &GENERATORS).expect("commit failed");

        let kc1 = pc.to_key_commitment();
        let kc2 = pc.to_key_commitment();
        assert_eq!(kc1, kc2);

        // The KeyCommitment inner bytes must equal the compressed point bytes.
        let compressed = pc.to_compressed();
        assert_eq!(kc1.0, B256::from(compressed));
    }

    /// Exceeding MAX_AUTHORIZED_KEYS must return an error.
    #[test]
    fn too_many_keys_rejected() {
        let hashes: Vec<U256> = (0..=MAX_AUTHORIZED_KEYS as u64).map(U256::from).collect();
        let result = PedersenCommitment::commit(&hashes, U256::from(1), &GENERATORS);
        assert!(result.is_err());
    }

    /// Empty key set with only blinding should still work.
    #[test]
    fn empty_keys_with_blinding() {
        let c1 = PedersenCommitment::commit(&[], U256::from(42), &GENERATORS)
            .expect("commit should succeed with empty keys");
        let c2 = PedersenCommitment::commit(&[], U256::from(43), &GENERATORS)
            .expect("commit should succeed with empty keys");
        // Different blinding factors still produce different commitments
        assert_ne!(c1, c2);
    }

    /// Zero blinding with zero keys produces the identity point.
    #[test]
    fn zero_blinding_zero_keys_is_identity() {
        use ark_ec::AffineRepr;
        let c = PedersenCommitment::commit(&[], U256::ZERO, &GENERATORS)
            .expect("commit should succeed");
        // The point should be the identity (point at infinity)
        assert!(c.point.into_affine().is_zero());
    }
}

#[cfg(test)]
mod membership_tests {
    use super::*;
    use ark_std::rand::SeedableRng;

    /// Deterministic RNG for reproducible tests.
    fn test_rng() -> ark_std::rand::rngs::StdRng {
        ark_std::rand::rngs::StdRng::seed_from_u64(0xDEAD_BEEF)
    }

    fn sample_key_hashes(n: usize) -> Vec<U256> {
        (1..=n as u64).map(|i| U256::from(i * 1000 + 7)).collect()
    }

    const TEST_BLINDING: U256 = U256::from_limbs([42, 0, 0, 0]);

    /// Prove and verify membership at a single index -- the happy path.
    #[test]
    fn test_membership_proof_valid() {
        let key_hashes = sample_key_hashes(5);
        let gens = &*GENERATORS;
        let commitment =
            PedersenCommitment::commit(&key_hashes, TEST_BLINDING, gens).expect("commit");

        let mut rng = test_rng();
        let proof =
            MembershipProof::prove(&key_hashes, TEST_BLINDING, 2, gens, &mut rng).expect("prove");

        assert!(
            proof.verify(&commitment, key_hashes[2], key_hashes.len(), gens),
            "valid proof must verify"
        );
    }

    /// Verification must fail when the claimed value is wrong.
    #[test]
    fn test_membership_proof_wrong_value() {
        let key_hashes = sample_key_hashes(4);
        let gens = &*GENERATORS;
        let commitment =
            PedersenCommitment::commit(&key_hashes, TEST_BLINDING, gens).expect("commit");

        let mut rng = test_rng();
        let proof =
            MembershipProof::prove(&key_hashes, TEST_BLINDING, 1, gens, &mut rng).expect("prove");

        // Claim a different value for slot 1.
        let wrong_value = U256::from(0xBAD);
        assert!(
            !proof.verify(&commitment, wrong_value, key_hashes.len(), gens),
            "wrong claimed value must not verify"
        );
    }

    /// Verification must fail when the key_index in the proof doesn't match
    /// where the value actually lives.
    #[test]
    fn test_membership_proof_wrong_index() {
        let key_hashes = sample_key_hashes(4);
        let gens = &*GENERATORS;
        let commitment =
            PedersenCommitment::commit(&key_hashes, TEST_BLINDING, gens).expect("commit");

        let mut rng = test_rng();
        // Prove membership at index 0.
        let proof =
            MembershipProof::prove(&key_hashes, TEST_BLINDING, 0, gens, &mut rng).expect("prove");

        // Try to verify with the value from slot 1 instead.
        assert!(
            !proof.verify(&commitment, key_hashes[1], key_hashes.len(), gens),
            "proof for index 0 must not verify with value from index 1"
        );
    }

    /// Verification must fail against a different commitment.
    #[test]
    fn test_membership_proof_wrong_commitment() {
        let key_hashes = sample_key_hashes(3);
        let gens = &*GENERATORS;

        let mut rng = test_rng();
        let proof =
            MembershipProof::prove(&key_hashes, TEST_BLINDING, 0, gens, &mut rng).expect("prove");

        // Build a commitment with different blinding.
        let other_commitment =
            PedersenCommitment::commit(&key_hashes, U256::from(999), gens).expect("commit");

        assert!(
            !proof.verify(&other_commitment, key_hashes[0], key_hashes.len(), gens),
            "proof must not verify against a different commitment"
        );
    }

    /// Prove membership for every position in a key set.
    #[test]
    fn test_membership_proof_all_positions() {
        let key_hashes = sample_key_hashes(7);
        let gens = &*GENERATORS;
        let commitment =
            PedersenCommitment::commit(&key_hashes, TEST_BLINDING, gens).expect("commit");

        let mut rng = test_rng();
        for idx in 0..key_hashes.len() {
            let proof = MembershipProof::prove(&key_hashes, TEST_BLINDING, idx, gens, &mut rng)
                .expect("prove");

            assert!(
                proof.verify(&commitment, key_hashes[idx], key_hashes.len(), gens),
                "proof at index {idx} must verify"
            );

            // Cross-check: wrong value at same index must fail.
            assert!(
                !proof.verify(&commitment, U256::from(0xDEAD), key_hashes.len(), gens),
                "proof at index {idx} must not verify with wrong value"
            );
        }
    }

    /// Single-key set edge case.
    #[test]
    fn test_membership_proof_single_key() {
        let key_hashes = vec![U256::from(42)];
        let gens = &*GENERATORS;
        let commitment =
            PedersenCommitment::commit(&key_hashes, TEST_BLINDING, gens).expect("commit");

        let mut rng = test_rng();
        let proof =
            MembershipProof::prove(&key_hashes, TEST_BLINDING, 0, gens, &mut rng).expect("prove");

        assert!(proof.verify(&commitment, key_hashes[0], 1, gens));
        assert!(!proof.verify(&commitment, U256::from(43), 1, gens));
    }

    /// Maximum key set (20 keys).
    #[test]
    fn test_membership_proof_max_keys() {
        let key_hashes = sample_key_hashes(MAX_AUTHORIZED_KEYS);
        let gens = &*GENERATORS;
        let commitment =
            PedersenCommitment::commit(&key_hashes, TEST_BLINDING, gens).expect("commit");

        let mut rng = test_rng();
        // Prove at the last index.
        let last = MAX_AUTHORIZED_KEYS - 1;
        let proof = MembershipProof::prove(&key_hashes, TEST_BLINDING, last, gens, &mut rng)
            .expect("prove");

        assert!(proof.verify(&commitment, key_hashes[last], key_hashes.len(), gens));
    }

    /// RLP roundtrip for the membership proof.
    #[test]
    fn test_membership_proof_rlp_roundtrip() {
        let key_hashes = sample_key_hashes(4);
        let gens = &*GENERATORS;

        let mut rng = test_rng();
        let proof =
            MembershipProof::prove(&key_hashes, TEST_BLINDING, 2, gens, &mut rng).expect("prove");

        let mut buf = Vec::new();
        proof.encode(&mut buf);
        let decoded =
            MembershipProof::decode(&mut buf.as_slice()).expect("RLP decode should succeed");

        assert_eq!(proof, decoded);

        // The decoded proof must still verify.
        let commitment =
            PedersenCommitment::commit(&key_hashes, TEST_BLINDING, gens).expect("commit");
        assert!(decoded.verify(&commitment, key_hashes[2], key_hashes.len(), gens));
    }

    /// `fr_to_u256` and `u256_to_fr` must roundtrip.
    #[test]
    fn test_fr_u256_roundtrip() {
        let values = [U256::ZERO, U256::from(1), U256::from(42), U256::MAX];
        for v in values {
            let fr = u256_to_fr(v);
            let back = fr_to_u256(fr);
            // The roundtrip equals v mod r, so re-convert and compare field elements.
            let fr2 = u256_to_fr(back);
            assert_eq!(fr, fr2, "fr_to_u256 -> u256_to_fr roundtrip failed for {v}");
        }
    }

    /// Verify that num_keys mismatch is rejected.
    #[test]
    fn test_membership_proof_wrong_num_keys() {
        let key_hashes = sample_key_hashes(4);
        let gens = &*GENERATORS;
        let commitment =
            PedersenCommitment::commit(&key_hashes, TEST_BLINDING, gens).expect("commit");

        let mut rng = test_rng();
        let proof =
            MembershipProof::prove(&key_hashes, TEST_BLINDING, 0, gens, &mut rng).expect("prove");

        // Pass wrong num_keys (too large) — responses.len() won't match.
        assert!(!proof.verify(&commitment, key_hashes[0], 10, gens));
        // Pass wrong num_keys (too small).
        assert!(!proof.verify(&commitment, key_hashes[0], 2, gens));
    }
}
