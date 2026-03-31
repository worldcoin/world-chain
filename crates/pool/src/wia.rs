//! World ID Account (WIA) transaction pool validation.
//!
//! This module validates EIP-2718 type `0x6f` transactions at pool entry,
//! following the same pattern as PBH transaction validation.

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use thiserror::Error;
use world_chain_primitives::wia::{
    key_slot, AuthType, AuthorizedKey, SignedTxWorldId, WorldIdSignature, MAX_AUTHORIZED_KEYS,
    NUM_KEYS_SLOT, WORLD_ID_ACCOUNT_FACTORY, WORLD_TX_TYPE,
};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum WiaValidationError {
    #[error("invalid 0x6f transaction encoding: {0}")]
    InvalidEncoding(String),
    #[error("account not initialized (no storage at derived address)")]
    AccountNotInitialized,
    #[error("invalid key count in account storage: {0}")]
    InvalidKeyCount(usize),
    #[error("missing key data in account storage for key {0}")]
    MissingKeyData(usize),
    #[error("invalid auth type byte: 0x{0:02x}")]
    InvalidAuthType(u8),
    #[error("no authorized key matches signature type {0:?}")]
    NoMatchingKey(AuthType),
    #[error("signature verification failed")]
    InvalidSignature,
    #[error("WebAuthn challenge mismatch")]
    WebAuthnChallengeMismatch,
    #[error("WebAuthn client_data_json parse error: {0}")]
    WebAuthnParseError(String),
    #[error("non-empty authorization_list in WIA transaction (security violation)")]
    NonEmptyAuthorizationList,
    #[error("provider error: {0}")]
    Provider(String),
}

// ---------------------------------------------------------------------------
// Validated output
// ---------------------------------------------------------------------------

/// A successfully validated World ID Account transaction with its derived sender.
pub struct ValidatedWiaTx {
    pub signed_tx: SignedTxWorldId,
    pub sender: Address,
}

// ---------------------------------------------------------------------------
// Main validation entry point
// ---------------------------------------------------------------------------

/// Validate a raw `0x6f` transaction against on-chain state.
///
/// Steps:
/// 1. Verify the type byte is `0x6f`
/// 2. Decode the EIP-2718 envelope
/// 3. Reject non-empty `authorization_list` (prevents re-delegation away from
///    `WorldIDAccountDelegate`)
/// 4. Derive sender from `account_nullifier`
/// 5. Read authorized keys from account storage
/// 6. Verify the signature
/// 7. Return validated tx + sender
pub fn validate_wia_transaction<S: reth_storage_api::StateProvider>(
    state: &S,
    raw_tx: &[u8],
) -> Result<ValidatedWiaTx, WiaValidationError> {
    if raw_tx.first() != Some(&WORLD_TX_TYPE) {
        return Err(WiaValidationError::InvalidEncoding(
            "not a 0x6f transaction".to_string(),
        ));
    }

    let signed_tx = SignedTxWorldId::decode_2718(&mut &raw_tx[1..])
        .map_err(|e| WiaValidationError::InvalidEncoding(e.to_string()))?;

    // Security: reject any 0x6f tx that carries an authorization_list.
    // A non-empty list could re-delegate the account away from WorldIDAccountDelegate.
    // TODO: More nuanced handling may be needed for future upgrades.
    if !signed_tx.tx.authorization_list.is_empty() {
        return Err(WiaValidationError::NonEmptyAuthorizationList);
    }

    let sender = signed_tx.tx.derive_sender();
    let keys = read_authorized_keys(state, WORLD_ID_ACCOUNT_FACTORY, sender)?;
    let signing_hash = signed_tx.tx.signing_hash();
    let valid = verify_world_id_signature(signing_hash, &signed_tx.signature, &keys)?;

    if !valid {
        return Err(WiaValidationError::InvalidSignature);
    }

    Ok(ValidatedWiaTx { signed_tx, sender })
}

// ---------------------------------------------------------------------------
// Storage reading
// ---------------------------------------------------------------------------

/// Read the authorized keys for a WIA account from on-chain state.
///
/// Attempts to read from the account's own storage first (precompile path).
/// Falls back with `AccountNotInitialized` if no storage is found.
///
/// TODO: Add factory-storage reading path for the reference Solidity
/// implementation once the exact storage layout is finalised.
pub fn read_authorized_keys<S: reth_storage_api::StateProvider>(
    state: &S,
    _factory: Address,
    account: Address,
) -> Result<Vec<AuthorizedKey>, WiaValidationError> {
    let num_keys_raw = state
        .storage(account, *NUM_KEYS_SLOT)
        .map_err(|e| WiaValidationError::Provider(e.to_string()))?;

    match num_keys_raw {
        Some(v) => read_keys_from_account_storage(state, account, v),
        None => Err(WiaValidationError::AccountNotInitialized),
    }
}

fn read_keys_from_account_storage<S: reth_storage_api::StateProvider>(
    state: &S,
    account: Address,
    num_keys_raw: U256,
) -> Result<Vec<AuthorizedKey>, WiaValidationError> {
    let num_keys: usize = num_keys_raw
        .try_into()
        .map_err(|_| WiaValidationError::InvalidKeyCount(usize::MAX))?;

    if num_keys == 0 || num_keys > MAX_AUTHORIZED_KEYS {
        return Err(WiaValidationError::InvalidKeyCount(num_keys));
    }

    let mut keys = Vec::with_capacity(num_keys);
    for i in 0..num_keys {
        let slot = key_slot(U256::from(i));

        let packed = state
            .storage(account, slot)
            .map_err(|e| WiaValidationError::Provider(e.to_string()))?
            .ok_or(WiaValidationError::MissingKeyData(i))?;

        let auth_type_byte: u8 = (packed & U256::from(0xFFu8))
            .try_into()
            .map_err(|_| WiaValidationError::InvalidAuthType(0xFF))?;
        let auth_type = AuthType::try_from(auth_type_byte)
            .map_err(|_| WiaValidationError::InvalidAuthType(auth_type_byte))?;

        let key_len_u256: U256 = (packed >> 8) & U256::from(0xFFFFu32);
        let key_len: usize = key_len_u256
            .try_into()
            .map_err(|_| WiaValidationError::MissingKeyData(i))?;

        // Key bytes live in the next consecutive slot
        let next_slot = B256::from(U256::from_be_bytes(slot.0) + U256::from(1u8));
        let key_bytes_raw = state
            .storage(account, next_slot)
            .map_err(|e| WiaValidationError::Provider(e.to_string()))?
            .ok_or(WiaValidationError::MissingKeyData(i))?;

        let raw = key_bytes_raw.to_be_bytes::<32>();
        let key_data = Bytes::copy_from_slice(&raw[..key_len.min(32)]);

        keys.push(AuthorizedKey { auth_type, key_data });
    }

    Ok(keys)
}

// ---------------------------------------------------------------------------
// Signature verification dispatch
// ---------------------------------------------------------------------------

/// Verify a `WorldIdSignature` against `signing_hash` using the first matching
/// key from `keys`.
pub fn verify_world_id_signature(
    signing_hash: B256,
    signature: &WorldIdSignature,
    keys: &[AuthorizedKey],
) -> Result<bool, WiaValidationError> {
    let sig_type = signature.auth_type();

    let key = keys
        .iter()
        .find(|k| k.auth_type == sig_type)
        .ok_or(WiaValidationError::NoMatchingKey(sig_type))?;

    match signature {
        WorldIdSignature::Secp256k1 { y_parity, r, s } => {
            Ok(verify_secp256k1(signing_hash, &key.key_data, *y_parity, *r, *s))
        }
        WorldIdSignature::P256 { r, s } => Ok(verify_p256(signing_hash, &key.key_data, *r, *s)),
        WorldIdSignature::WebAuthn {
            authenticator_data,
            client_data_json,
            r,
            s,
        } => verify_webauthn(
            signing_hash,
            &key.key_data,
            authenticator_data,
            client_data_json,
            *r,
            *s,
        ),
    }
}

// ---------------------------------------------------------------------------
// secp256k1
// ---------------------------------------------------------------------------

/// Verify a secp256k1 ECDSA signature.
///
/// Recovers the signer address from `(signing_hash, y_parity, r, s)` and
/// compares it against the Ethereum address derived from `pubkey` (a
/// compressed 33-byte SEC1-encoded point).
pub fn verify_secp256k1(
    signing_hash: B256,
    pubkey: &[u8],
    y_parity: bool,
    r: U256,
    s: U256,
) -> bool {
    use alloy_primitives::Signature;
    use k256::ecdsa::VerifyingKey;

    if pubkey.len() != 33 {
        return false;
    }

    let r_b256 = B256::from(r);
    let s_b256 = B256::from(s);
    let sig = Signature::from_scalars_and_parity(r_b256, s_b256, y_parity);
    let recovered = match sig.recover_address_from_prehash(&signing_hash) {
        Ok(a) => a,
        Err(_) => return false,
    };

    // Derive the expected address from the stored compressed pubkey.
    let vk = match VerifyingKey::from_sec1_bytes(pubkey) {
        Ok(vk) => vk,
        Err(_) => return false,
    };
    let uncompressed = vk.to_encoded_point(false);
    let bytes = uncompressed.as_bytes();
    if bytes.len() != 65 {
        return false;
    }
    let hash = keccak256(&bytes[1..]);
    let expected = Address::from_slice(&hash[12..]);

    recovered == expected
}

// ---------------------------------------------------------------------------
// P-256
// ---------------------------------------------------------------------------

/// Verify a NIST P-256 ECDSA signature.
///
/// `pubkey` must be the raw 64-byte uncompressed point (x ‖ y, no `0x04`
/// prefix).  `r` and `s` are big-endian 32-byte scalars.
pub fn verify_p256(signing_hash: B256, pubkey: &[u8], r: U256, s: U256) -> bool {
    use p256::{
        ecdsa::{signature::hazmat::PrehashVerifier, Signature, VerifyingKey},
        EncodedPoint,
    };

    if pubkey.len() != 64 {
        return false;
    }

    let mut uncompressed = Vec::with_capacity(65);
    uncompressed.push(0x04);
    uncompressed.extend_from_slice(pubkey);

    let ep = match EncodedPoint::from_bytes(&uncompressed) {
        Ok(ep) => ep,
        Err(_) => return false,
    };
    let vk = match VerifyingKey::from_encoded_point(&ep) {
        Ok(vk) => vk,
        Err(_) => return false,
    };

    let r_bytes = r.to_be_bytes::<32>();
    let s_bytes = s.to_be_bytes::<32>();
    let sig = match Signature::from_scalars(
        *p256::FieldBytes::from_slice(&r_bytes),
        *p256::FieldBytes::from_slice(&s_bytes),
    ) {
        Ok(s) => s,
        Err(_) => return false,
    };

    vk.verify_prehash(signing_hash.as_slice(), &sig).is_ok()
}

// ---------------------------------------------------------------------------
// WebAuthn
// ---------------------------------------------------------------------------

/// Verify a WebAuthn signature (P-256 under the WebAuthn ceremony).
///
/// Algorithm:
/// 1. Decode `client_data_json`, extract the `"challenge"` field.
/// 2. Verify `challenge == base64url(signing_hash)`.
/// 3. Compute `message = SHA-256(authenticator_data ‖ SHA-256(client_data_json))`.
/// 4. Verify P-256 signature over `message`.
pub fn verify_webauthn(
    signing_hash: B256,
    pubkey: &[u8],
    authenticator_data: &[u8],
    client_data_json: &[u8],
    r: U256,
    s: U256,
) -> Result<bool, WiaValidationError> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use sha2::{Digest, Sha256};

    let client_data_str = std::str::from_utf8(client_data_json)
        .map_err(|e| WiaValidationError::WebAuthnParseError(e.to_string()))?;

    let challenge_b64 = extract_json_string_field(client_data_str, "challenge")
        .ok_or_else(|| {
            WiaValidationError::WebAuthnParseError("missing challenge field".to_string())
        })?;

    let challenge_bytes = URL_SAFE_NO_PAD
        .decode(challenge_b64)
        .map_err(|e| WiaValidationError::WebAuthnParseError(e.to_string()))?;

    if challenge_bytes != signing_hash.as_slice() {
        return Err(WiaValidationError::WebAuthnChallengeMismatch);
    }

    let cdj_hash = Sha256::digest(client_data_json);
    let mut preimage = Vec::with_capacity(authenticator_data.len() + 32);
    preimage.extend_from_slice(authenticator_data);
    preimage.extend_from_slice(&cdj_hash);
    let message = Sha256::digest(&preimage);

    use p256::{
        ecdsa::{signature::hazmat::PrehashVerifier, Signature, VerifyingKey},
        EncodedPoint,
    };

    if pubkey.len() != 64 {
        return Ok(false);
    }
    let mut uncompressed = Vec::with_capacity(65);
    uncompressed.push(0x04);
    uncompressed.extend_from_slice(pubkey);

    let ep = match EncodedPoint::from_bytes(&uncompressed) {
        Ok(ep) => ep,
        Err(_) => return Ok(false),
    };
    let vk = match VerifyingKey::from_encoded_point(&ep) {
        Ok(vk) => vk,
        Err(_) => return Ok(false),
    };

    let r_bytes = r.to_be_bytes::<32>();
    let s_bytes = s.to_be_bytes::<32>();
    let sig = match Signature::from_scalars(
        *p256::FieldBytes::from_slice(&r_bytes),
        *p256::FieldBytes::from_slice(&s_bytes),
    ) {
        Ok(s) => s,
        Err(_) => return Ok(false),
    };

    Ok(vk.verify_prehash(message.as_slice(), &sig).is_ok())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Minimal JSON string-field extractor — avoids pulling in `serde_json`.
/// Returns the raw (unquoted) value of the first occurrence of `"field": "…"`.
fn extract_json_string_field<'a>(json: &'a str, field: &str) -> Option<&'a str> {
    let key = format!("\"{}\":", field);
    let start = json.find(&key)? + key.len();
    let rest = json[start..].trim_start();
    // Expect a quoted string value
    if !rest.starts_with('"') {
        return None;
    }
    let inner = &rest[1..];
    let end = inner.find('"')?;
    Some(&inner[..end])
}
