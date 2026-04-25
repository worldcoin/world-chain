//! WIP-1001 signature verification.
//!
//! Generalized verification across all schemes defined in the
//! [WIP-1001](https://github.com/worldcoin/world-chain/blob/main/wips/wip-1001.md)
//! envelope: secp256k1, P-256, WebAuthn, and Ed25519.
//!
//! For non-recoverable schemes (P-256, WebAuthn, Ed25519), the session pubkey
//! is read from the transaction's `session_key` field; for secp256k1, the
//! recovered pubkey is checked against `session_key`.

use alloy_consensus::crypto::RecoveryError;
use alloy_primitives::{uint, B256, U256};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use p256::{
    ecdsa::{signature::hazmat::PrehashVerifier, Signature as P256EcdsaSignature, VerifyingKey},
    EncodedPoint,
};
use sha2::{Digest, Sha256};

use crate::transaction::{
    signature::SessionKeyError, P256Signature, SessionKey, WebAuthnSignature, Wip1001Signature,
};

/// The P-256 (secp256r1) curve order n.
pub const P256_ORDER: U256 =
    uint!(0xFFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632551_U256);

/// `n / 2`. Signatures with `s > n/2` are rejected as non-canonical (low-s rule)
/// to prevent signature malleability.
pub const P256N_HALF: U256 =
    uint!(0x7FFFFFFF800000007FFFFFFFFFFFFFFFDE737D56D38BCF4279DCE5617E3192A8_U256);

/// Minimum WebAuthn `authenticatorData` length: 32-byte rpIdHash + 1-byte flags
/// + 4-byte signCount.
const MIN_AUTH_DATA_LEN: usize = 37;

// `authenticatorData` flag bits, byte 32. ref: <https://www.w3.org/TR/webauthn-2/#sctn-authenticator-data>
const FLAG_UP: u8 = 0x01; // User Presence
const FLAG_UV: u8 = 0x04; // User Verified
const FLAG_AT: u8 = 0x40; // Attested credential data — must be unset for assertions
const FLAG_ED: u8 = 0x80; // Extension data — unsupported

/// Errors returned by [`verify_wip1001_signature`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Wip1001VerifyError {
    /// The declared `signature_type` byte and the [`Wip1001Signature`] variant
    /// disagree.
    #[error("signature_type mismatch: tx={tx:#04x} sig={sig:#04x}")]
    SignatureTypeMismatch {
        /// `signature_type` field on the transaction.
        tx: u8,
        /// Discriminator on the [`Wip1001Signature`] variant.
        sig: u8,
    },
    /// `session_key` bytes are malformed for the declared `signature_type`.
    #[error(transparent)]
    InvalidSessionKey(#[from] SessionKeyError),
    /// secp256k1 ECDSA recovery failed.
    #[error("secp256k1 recovery failed")]
    Secp256k1RecoveryFailed,
    /// Recovered secp256k1 pubkey did not match the declared `session_key`.
    #[error("secp256k1 recovered pubkey does not match session_key")]
    Secp256k1KeyMismatch,
    /// P-256 signature failed cryptographic verification.
    #[error("P-256 verification failed: {0}")]
    P256VerificationFailed(&'static str),
    /// WebAuthn `authenticatorData` / `clientDataJSON` failed validation.
    #[error("WebAuthn validation failed: {0}")]
    WebAuthnInvalid(&'static str),
    /// Ed25519 EdDSA signature failed `verify_strict`.
    #[error("EdDSA verification failed")]
    EddsaVerificationFailed,
}

impl From<Wip1001VerifyError> for RecoveryError {
    fn from(_: Wip1001VerifyError) -> Self {
        Self::new()
    }
}

/// Verify a WIP-1001 signature against the supplied `signing_hash` and
/// `session_key`.
///
/// On success, returns the typed [`SessionKey`] that was bound to the
/// signature. The caller is responsible for invoking
/// `IWorldIDKeyRing.isAuthorized(keyring, session_key)` separately.
pub fn verify_wip1001_signature(
    signature_type: u8,
    session_key: &[u8],
    signature: &Wip1001Signature,
    signing_hash: &B256,
) -> Result<SessionKey, Wip1001VerifyError> {
    if signature.signature_type() != signature_type {
        return Err(Wip1001VerifyError::SignatureTypeMismatch {
            tx: signature_type,
            sig: signature.signature_type(),
        });
    }

    let session_key = SessionKey::from_wire(signature_type, session_key)?;

    match (&session_key, signature) {
        (SessionKey::Secp256k1(declared_pubkey), Wip1001Signature::Secp256k1(sig)) => {
            verify_secp256k1(declared_pubkey, sig, signing_hash)?;
        }
        (SessionKey::P256 { x, y }, Wip1001Signature::P256(sig)) => {
            verify_p256(sig, x, y, signing_hash)?;
        }
        (SessionKey::WebAuthn { x, y }, Wip1001Signature::WebAuthn(sig)) => {
            verify_webauthn(sig, x, y, signing_hash)?;
        }
        (SessionKey::EdDSA(verifying_key), Wip1001Signature::EdDSA(sig)) => {
            verify_eddsa(verifying_key, sig, signing_hash)?;
        }
        // Already covered by the discriminator equality check above.
        _ => unreachable!("signature_type was checked equal to session key type"),
    }

    Ok(session_key)
}

/// Recover the secp256k1 pubkey from `(sig, signing_hash)` and assert it
/// matches the declared compressed pubkey.
fn verify_secp256k1(
    declared_pubkey: &[u8; 33],
    sig: &alloy_primitives::Signature,
    signing_hash: &B256,
) -> Result<(), Wip1001VerifyError> {
    use alloy_consensus::crypto::secp256k1;

    // Recover the uncompressed (x, y) Ethereum-style pubkey, then re-encode in
    // SEC1-compressed form and compare. We cannot use ecrecover -> Address here
    // because the spec authorizes on the compressed pubkey, not its keccak256
    // image.
    let _addr = secp256k1::recover_signer(sig, *signing_hash)
        .map_err(|_| Wip1001VerifyError::Secp256k1RecoveryFailed)?;

    let recovered_compressed = recover_secp256k1_compressed(sig, signing_hash)
        .ok_or(Wip1001VerifyError::Secp256k1RecoveryFailed)?;

    if recovered_compressed != *declared_pubkey {
        return Err(Wip1001VerifyError::Secp256k1KeyMismatch);
    }
    Ok(())
}

/// Recover the SEC1-compressed (33-byte) secp256k1 pubkey from a signature.
fn recover_secp256k1_compressed(
    sig: &alloy_primitives::Signature,
    signing_hash: &B256,
) -> Option<[u8; 33]> {
    let verifying_key = sig.recover_from_prehash(signing_hash).ok()?;
    let encoded = verifying_key.to_encoded_point(true);
    let bytes = encoded.as_bytes();
    if bytes.len() != 33 {
        return None;
    }
    let mut out = [0u8; 33];
    out.copy_from_slice(bytes);
    Some(out)
}

/// Verify a raw P-256 ECDSA signature against `signing_hash` using the supplied
/// `(x, y)` public key. Enforces the low-s canonicalization rule.
pub fn verify_p256(
    sig: &P256Signature,
    pub_key_x: &B256,
    pub_key_y: &B256,
    signing_hash: &B256,
) -> Result<(), Wip1001VerifyError> {
    if U256::from_be_bytes(sig.s.0) > P256N_HALF {
        return Err(Wip1001VerifyError::P256VerificationFailed(
            "s value above n/2 (low-s rule)",
        ));
    }

    let encoded_point = EncodedPoint::from_affine_coordinates(
        pub_key_x.0.as_slice().into(),
        pub_key_y.0.as_slice().into(),
        false,
    );
    let verifying_key = VerifyingKey::from_encoded_point(&encoded_point)
        .map_err(|_| Wip1001VerifyError::P256VerificationFailed("invalid pubkey"))?;

    let mut sig_bytes = [0u8; 64];
    sig_bytes[..32].copy_from_slice(sig.r.as_slice());
    sig_bytes[32..].copy_from_slice(sig.s.as_slice());
    let p256_sig = P256EcdsaSignature::from_slice(&sig_bytes)
        .map_err(|_| Wip1001VerifyError::P256VerificationFailed("invalid (r, s)"))?;

    verifying_key
        .verify_prehash(signing_hash.as_slice(), &p256_sig)
        .map_err(|_| Wip1001VerifyError::P256VerificationFailed("ecdsa verify"))
}

/// Verify a WebAuthn assertion: parse `authenticatorData` flags, validate
/// `clientDataJSON` (`type == "webauthn.get"`, `challenge == base64url(signing_hash)`),
/// then verify the inner P-256 signature over
/// `sha256(authenticator_data || sha256(client_data_json))`.
pub fn verify_webauthn(
    sig: &WebAuthnSignature,
    pub_key_x: &B256,
    pub_key_y: &B256,
    signing_hash: &B256,
) -> Result<(), Wip1001VerifyError> {
    let auth = sig.authenticator_data.as_ref();
    let cdj = sig.client_data_json.as_ref();
    let message_hash = compute_webauthn_message_hash(auth, cdj, signing_hash)?;

    let p256_sig = P256Signature { r: sig.r, s: sig.s };
    verify_p256(&p256_sig, pub_key_x, pub_key_y, &message_hash)
}

/// Verify an Ed25519 EdDSA signature against `signing_hash` using the supplied
/// `verifying_key`.
///
/// Treats `signing_hash` as the *message* (not a prehash) — Ed25519 internally
/// hashes its input via SHA-512 as part of the signing/verification equation.
/// `verify_strict` rejects mixed-order public keys and small-subgroup signatures.
pub fn verify_eddsa(
    verifying_key: &ed25519_dalek::VerifyingKey,
    sig: &ed25519_dalek::Signature,
    signing_hash: &B256,
) -> Result<(), Wip1001VerifyError> {
    verifying_key
        .verify_strict(signing_hash.as_slice(), sig)
        .map_err(|_| Wip1001VerifyError::EddsaVerificationFailed)
}

fn compute_webauthn_message_hash(
    authenticator_data: &[u8],
    client_data_json: &[u8],
    signing_hash: &B256,
) -> Result<B256, Wip1001VerifyError> {
    if authenticator_data.len() < MIN_AUTH_DATA_LEN {
        return Err(Wip1001VerifyError::WebAuthnInvalid(
            "authenticatorData too short",
        ));
    }
    let flags = authenticator_data[32];
    let (up, uv, at, ed) = (
        flags & FLAG_UP,
        flags & FLAG_UV,
        flags & FLAG_AT,
        flags & FLAG_ED,
    );
    if up == 0 && uv == 0 {
        return Err(Wip1001VerifyError::WebAuthnInvalid(
            "neither UP nor UV flag set",
        ));
    }
    if at != 0 {
        return Err(Wip1001VerifyError::WebAuthnInvalid(
            "AT flag must not be set on assertion",
        ));
    }
    if ed != 0 {
        return Err(Wip1001VerifyError::WebAuthnInvalid(
            "ED (extension) flag unsupported",
        ));
    }
    if authenticator_data.len() != MIN_AUTH_DATA_LEN {
        return Err(Wip1001VerifyError::WebAuthnInvalid(
            "authenticatorData has unexpected trailing bytes",
        ));
    }

    let client_data: ClientDataJson<'_> = serde_json::from_slice(client_data_json)
        .map_err(|_| Wip1001VerifyError::WebAuthnInvalid("clientDataJSON malformed"))?;
    if client_data.type_field != "webauthn.get" {
        return Err(Wip1001VerifyError::WebAuthnInvalid(
            "clientDataJSON.type != \"webauthn.get\"",
        ));
    }
    let expected_challenge = URL_SAFE_NO_PAD.encode(signing_hash.as_slice());
    if client_data.challenge != expected_challenge {
        return Err(Wip1001VerifyError::WebAuthnInvalid(
            "clientDataJSON.challenge does not match signing_hash",
        ));
    }

    let cdj_hash = Sha256::digest(client_data_json);
    let mut hasher = Sha256::new();
    hasher.update(authenticator_data);
    hasher.update(cdj_hash);
    Ok(B256::from_slice(&hasher.finalize()))
}

#[derive(serde::Deserialize)]
struct ClientDataJson<'a> {
    #[serde(rename = "type")]
    type_field: &'a str,
    challenge: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Bytes;
    use p256::{
        ecdsa::{signature::hazmat::PrehashSigner, SigningKey as P256SigningKey},
        elliptic_curve::rand_core::OsRng,
    };

    /// Returns `(signing_key, x, y)`.
    fn p256_keypair() -> (P256SigningKey, B256, B256) {
        let signing_key = P256SigningKey::random(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        let encoded = verifying_key.to_encoded_point(false);
        let x = B256::from_slice(encoded.x().expect("x").as_ref());
        let y = B256::from_slice(encoded.y().expect("y").as_ref());
        (signing_key, x, y)
    }

    /// Signs `hash` with low-s normalization.
    fn p256_sign(signing_key: &P256SigningKey, hash: &B256) -> P256Signature {
        let signature: p256::ecdsa::Signature = signing_key
            .sign_prehash(hash.as_slice())
            .expect("p256 sign");
        let bytes = signature.to_bytes();
        let r = B256::from_slice(&bytes[..32]);
        let s = U256::from_be_slice(&bytes[32..]);
        let s = if s > P256N_HALF { P256_ORDER - s } else { s };
        P256Signature {
            r,
            s: B256::from(s.to_be_bytes::<32>()),
        }
    }

    #[test]
    fn p256_happy_path() {
        let (key, x, y) = p256_keypair();
        let hash = B256::from_slice(&[0xAB; 32]);
        let sig = p256_sign(&key, &hash);
        verify_p256(&sig, &x, &y, &hash).expect("valid p256 signature");
    }

    #[test]
    fn p256_rejects_high_s() {
        let (key, x, y) = p256_keypair();
        let hash = B256::from_slice(&[0xCD; 32]);
        let sig = p256_sign(&key, &hash);
        // Flip s to its high-s counterpart; should now reject.
        let high_s = P256_ORDER - U256::from_be_bytes(sig.s.0);
        let bad = P256Signature {
            r: sig.r,
            s: B256::from(high_s.to_be_bytes::<32>()),
        };
        let err = verify_p256(&bad, &x, &y, &hash).unwrap_err();
        assert!(matches!(err, Wip1001VerifyError::P256VerificationFailed(_)));
    }

    #[test]
    fn p256_rejects_tampered_hash() {
        let (key, x, y) = p256_keypair();
        let hash = B256::from_slice(&[0x11; 32]);
        let sig = p256_sign(&key, &hash);
        let other = B256::from_slice(&[0x22; 32]);
        let err = verify_p256(&sig, &x, &y, &other).unwrap_err();
        assert!(matches!(err, Wip1001VerifyError::P256VerificationFailed(_)));
    }

    fn make_authenticator_data() -> Vec<u8> {
        let mut data = vec![0u8; MIN_AUTH_DATA_LEN];
        data[32] = FLAG_UP; // UP set, AT/ED clear
        data
    }

    fn make_client_data_json(challenge_b64: &str) -> Vec<u8> {
        format!(
            "{{\"type\":\"webauthn.get\",\"challenge\":\"{challenge_b64}\",\"origin\":\"https://example\"}}"
        )
        .into_bytes()
    }

    fn webauthn_message_hash(auth: &[u8], cdj: &[u8]) -> B256 {
        let cdj_hash = Sha256::digest(cdj);
        let mut hasher = Sha256::new();
        hasher.update(auth);
        hasher.update(cdj_hash);
        B256::from_slice(&hasher.finalize())
    }

    #[test]
    fn webauthn_happy_path() {
        let (key, x, y) = p256_keypair();
        let signing_hash = B256::from_slice(&[0x42; 32]);
        let challenge = URL_SAFE_NO_PAD.encode(signing_hash.as_slice());
        let auth = make_authenticator_data();
        let cdj = make_client_data_json(&challenge);
        let msg_hash = webauthn_message_hash(&auth, &cdj);
        let sig = p256_sign(&key, &msg_hash);

        let webauthn = WebAuthnSignature {
            authenticator_data: Bytes::from(auth),
            client_data_json: Bytes::from(cdj),
            r: sig.r,
            s: sig.s,
        };
        verify_webauthn(&webauthn, &x, &y, &signing_hash).expect("valid webauthn signature");
    }

    #[test]
    fn webauthn_rejects_bad_challenge() {
        let (key, x, y) = p256_keypair();
        let signing_hash = B256::from_slice(&[0x42; 32]);
        let bad_hash = B256::from_slice(&[0x43; 32]);
        let challenge = URL_SAFE_NO_PAD.encode(bad_hash.as_slice()); // wrong!
        let auth = make_authenticator_data();
        let cdj = make_client_data_json(&challenge);
        let msg_hash = webauthn_message_hash(&auth, &cdj);
        let sig = p256_sign(&key, &msg_hash);

        let webauthn = WebAuthnSignature {
            authenticator_data: Bytes::from(auth),
            client_data_json: Bytes::from(cdj),
            r: sig.r,
            s: sig.s,
        };
        let err = verify_webauthn(&webauthn, &x, &y, &signing_hash).unwrap_err();
        assert!(matches!(err, Wip1001VerifyError::WebAuthnInvalid(_)));
    }

    #[test]
    fn webauthn_rejects_missing_user_flags() {
        let (key, x, y) = p256_keypair();
        let signing_hash = B256::from_slice(&[0x42; 32]);
        let challenge = URL_SAFE_NO_PAD.encode(signing_hash.as_slice());
        let mut auth = make_authenticator_data();
        auth[32] = 0; // clear UP, no UV either
        let cdj = make_client_data_json(&challenge);
        let msg_hash = webauthn_message_hash(&auth, &cdj);
        let sig = p256_sign(&key, &msg_hash);

        let webauthn = WebAuthnSignature {
            authenticator_data: Bytes::from(auth),
            client_data_json: Bytes::from(cdj),
            r: sig.r,
            s: sig.s,
        };
        let err = verify_webauthn(&webauthn, &x, &y, &signing_hash).unwrap_err();
        assert!(matches!(err, Wip1001VerifyError::WebAuthnInvalid(_)));
    }

    #[test]
    fn webauthn_rejects_at_flag() {
        let (key, x, y) = p256_keypair();
        let signing_hash = B256::from_slice(&[0x42; 32]);
        let challenge = URL_SAFE_NO_PAD.encode(signing_hash.as_slice());
        let mut auth = make_authenticator_data();
        auth[32] |= FLAG_AT;
        let cdj = make_client_data_json(&challenge);
        let msg_hash = webauthn_message_hash(&auth, &cdj);
        let sig = p256_sign(&key, &msg_hash);

        let webauthn = WebAuthnSignature {
            authenticator_data: Bytes::from(auth),
            client_data_json: Bytes::from(cdj),
            r: sig.r,
            s: sig.s,
        };
        let err = verify_webauthn(&webauthn, &x, &y, &signing_hash).unwrap_err();
        assert!(matches!(err, Wip1001VerifyError::WebAuthnInvalid(_)));
    }

    /// Deterministic Ed25519 keypair from a fixed seed (no RNG dependency).
    fn ed25519_keypair() -> (ed25519_dalek::SigningKey, ed25519_dalek::VerifyingKey) {
        let seed: [u8; 32] = [
            0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec,
            0x2c, 0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03,
            0x1c, 0xae, 0x7f, 0x60,
        ];
        let sk = ed25519_dalek::SigningKey::from_bytes(&seed);
        let vk = sk.verifying_key();
        (sk, vk)
    }

    #[test]
    fn eddsa_happy_path() {
        use ed25519_dalek::Signer;
        let (sk, vk) = ed25519_keypair();
        let signing_hash = B256::from_slice(&[0x55; 32]);
        let sig = sk.sign(signing_hash.as_slice());
        verify_eddsa(&vk, &sig, &signing_hash).expect("valid eddsa signature");
    }

    #[test]
    fn eddsa_rejects_tampered_hash() {
        use ed25519_dalek::Signer;
        let (sk, vk) = ed25519_keypair();
        let signing_hash = B256::from_slice(&[0x55; 32]);
        let other = B256::from_slice(&[0x66; 32]);
        let sig = sk.sign(signing_hash.as_slice());
        let err = verify_eddsa(&vk, &sig, &other).unwrap_err();
        assert!(matches!(err, Wip1001VerifyError::EddsaVerificationFailed));
    }

    #[test]
    fn eddsa_rejects_wrong_pubkey() {
        use ed25519_dalek::Signer;
        let (sk_a, _vk_a) = ed25519_keypair();
        // Different seed -> different keypair.
        let sk_b = ed25519_dalek::SigningKey::from_bytes(&[0xCD; 32]);
        let vk_b = sk_b.verifying_key();
        let signing_hash = B256::from_slice(&[0x55; 32]);
        let sig = sk_a.sign(signing_hash.as_slice());
        // Verifying with someone else's pubkey must fail.
        let err = verify_eddsa(&vk_b, &sig, &signing_hash).unwrap_err();
        assert!(matches!(err, Wip1001VerifyError::EddsaVerificationFailed));
    }

    #[test]
    fn session_key_from_wire_rejects_wrong_length_eddsa() {
        let too_short = [0xAAu8; 31];
        let err = SessionKey::from_wire(Wip1001Signature::EDDSA_TYPE, &too_short);
        assert!(matches!(
            err,
            Err(SessionKeyError::InvalidLength {
                expected: 32,
                got: 31
            })
        ));
    }

    #[test]
    fn verify_wip1001_signature_eddsa_round_trip() {
        use ed25519_dalek::Signer;
        let (sk, vk) = ed25519_keypair();
        let signing_hash = B256::from_slice(&[0x77; 32]);
        let sig = sk.sign(signing_hash.as_slice());

        let key_bytes = vk.to_bytes();
        let signature = Wip1001Signature::EdDSA(sig);

        let session_key = verify_wip1001_signature(
            Wip1001Signature::EDDSA_TYPE,
            &key_bytes,
            &signature,
            &signing_hash,
        )
        .expect("verify");
        match session_key {
            SessionKey::EdDSA(recovered_vk) => assert_eq!(recovered_vk, vk),
            other => panic!("expected EdDSA SessionKey, got {other:?}"),
        }
    }
}
