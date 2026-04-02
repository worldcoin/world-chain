use alloy_primitives::{B256, U256, keccak256};
use alloy_rlp::{RlpDecodable, RlpEncodable};

use super::pedersen::{GENERATORS, PedersenCommitment};

pub const MAX_AUTHORIZED_KEYS: usize = 20;

/// A key hash is a BN254 scalar field element derived from the raw public key
/// bytes: `key_hash = to_field(keccak256(key_bytes))`.
pub type KeyHash = U256;

/// The BN254 (alt_bn128) scalar field order.
///
/// r = 21888242871839275222246405745257275088548364400416034343698204186575808495617
pub(crate) const BN254_ORDER: U256 = U256::from_limbs([
    0x43E1F593F0000001,
    0x2833E84879B97091,
    0xB85045B68181585D,
    0x30644E72E131A029,
]);

/// Reduce a 256-bit value into the BN254 scalar field.
#[inline]
pub fn to_field(v: U256) -> U256 {
    v.reduce_mod(BN254_ORDER)
}

/// Modular addition in the BN254 scalar field.
#[inline]
pub fn field_add(a: U256, b: U256) -> U256 {
    a.add_mod(b, BN254_ORDER)
}

/// Modular subtraction in the BN254 scalar field.
#[inline]
pub fn field_sub(a: U256, b: U256) -> U256 {
    if a >= b {
        (a - b).reduce_mod(BN254_ORDER)
    } else {
        BN254_ORDER - (b - a).reduce_mod(BN254_ORDER)
    }
}

/// Modular multiplication in the BN254 scalar field.
#[inline]
pub fn field_mul(a: U256, b: U256) -> U256 {
    a.mul_mod(b, BN254_ORDER)
}

/// Modular negation in the BN254 scalar field.
#[inline]
pub fn field_neg(a: U256) -> U256 {
    if a.is_zero() {
        U256::ZERO
    } else {
        BN254_ORDER - a
    }
}

/// Hash raw public key bytes into a BN254 scalar field element for use in
/// the Pedersen vector commitment.
///
/// `key_hash = to_field(keccak256(key_bytes))`
#[inline]
pub fn hash_key(key_bytes: &[u8]) -> KeyHash {
    to_field(U256::from_be_bytes(*keccak256(key_bytes)))
}

// ─── KeyCommitment ───────────────────────────────────────────────────────────

/// Commitment to the authorized key set -- the compressed BN254 G1 Pedersen
/// commitment point (32 bytes).
///
/// Embedded in the World ID Account address:
/// `addr = bytes20(keccak256(nu_a || C))`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, RlpEncodable, RlpDecodable)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct KeyCommitment(pub B256);

impl From<B256> for KeyCommitment {
    fn from(value: B256) -> Self {
        Self(value)
    }
}

// ─── Keychain ────────────────────────────────────────────────────────────────

/// A set of key hashes for a World ID Account.
///
/// Each entry is a BN254 scalar field element derived from the raw public key
/// bytes via [`hash_key`]. The Pedersen vector commitment over these hashes
/// (with a random blinding factor) produces a [`KeyCommitment`] that is
/// embedded in the account address.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct Keychain<const N: usize = MAX_AUTHORIZED_KEYS>([KeyHash; N]);

impl<const N: usize> Keychain<N> {
    pub fn new(keys: [KeyHash; N]) -> Self {
        Self(keys)
    }

    pub fn keys(&self) -> &[KeyHash; N] {
        &self.0
    }

    pub fn as_slice(&self) -> &[KeyHash] {
        &self.0
    }
}

impl<const N: usize> From<[KeyHash; N]> for Keychain<N> {
    fn from(value: [KeyHash; N]) -> Self {
        Self(value)
    }
}

impl<const N: usize> Keychain<N> {
    /// Compute the blinded Pedersen vector commitment over the key hashes.
    pub fn pedersen_commitment(&self, blinding_factor: U256) -> PedersenCommitment {
        let hashes: Vec<U256> = self.keys().iter().map(|k| to_field(*k)).collect();
        PedersenCommitment::commit(&hashes, blinding_factor, &GENERATORS)
            .expect("key set size is bounded by MAX_AUTHORIZED_KEYS")
    }

    /// Compute the key commitment (compressed G1 point bytes).
    pub fn key_commitment(&self, blinding_factor: U256) -> KeyCommitment {
        self.pedersen_commitment(blinding_factor)
            .to_key_commitment()
    }

    /// Signal hash for the account proof (section 3.3.3).
    ///
    /// `signalHash = keccak256(key_commitment) >> 8`
    pub fn pederson_hash(&self, blinding_factor: U256) -> U256 {
        let commitment = self.key_commitment(blinding_factor);
        let hash = keccak256(commitment.0.0);
        U256::from_be_bytes(*hash) >> 8
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_BLINDING: U256 = U256::from_limbs([7, 0, 0, 0]);

    fn sample_keychain() -> Keychain<3> {
        Keychain::new([
            hash_key(&[0x02; 33]),
            hash_key(&[0xAB; 64]),
            hash_key(&[0xEF; 32]),
        ])
    }

    #[test]
    fn test_pedersen_commitment_deterministic() {
        let kc = sample_keychain();
        let c1 = kc.pedersen_commitment(TEST_BLINDING);
        let c2 = kc.pedersen_commitment(TEST_BLINDING);
        assert_eq!(c1, c2);
    }

    #[test]
    fn test_key_commitment_deterministic() {
        let kc = sample_keychain();
        assert_eq!(
            kc.key_commitment(TEST_BLINDING),
            kc.key_commitment(TEST_BLINDING)
        );
    }

    #[test]
    fn test_commitment_differs_by_keys() {
        let kc1 = Keychain::new([hash_key(&[0x02; 33])]);
        let kc2 = Keychain::new([hash_key(&[0x03; 33])]);
        assert_ne!(
            kc1.key_commitment(TEST_BLINDING),
            kc2.key_commitment(TEST_BLINDING)
        );
    }

    #[test]
    fn test_commitment_differs_by_blinding() {
        let kc = Keychain::new([hash_key(&[0x02; 33])]);
        assert_ne!(
            kc.key_commitment(U256::from(1)),
            kc.key_commitment(U256::from(2))
        );
    }

    #[test]
    fn test_hash_key_deterministic() {
        let h1 = hash_key(&[0x02; 33]);
        let h2 = hash_key(&[0x02; 33]);
        assert_eq!(h1, h2);
        assert_ne!(hash_key(&[0x02; 33]), hash_key(&[0x03; 33]));
    }

    #[test]
    fn test_field_arithmetic() {
        let a = to_field(U256::from(10));
        let b = to_field(U256::from(20));
        assert_eq!(field_add(a, b), to_field(U256::from(30)));
        assert_eq!(field_mul(a, b), to_field(U256::from(200)));
        assert_eq!(field_sub(b, a), to_field(U256::from(10)));
        assert_eq!(field_add(a, field_neg(a)), U256::ZERO);
    }

    #[test]
    fn test_field_sub_underflow() {
        let a = to_field(U256::from(5));
        let b = to_field(U256::from(10));
        let result = field_sub(a, b);
        assert_eq!(field_add(result, b), a);
    }
}
