//! Authorization lookup against the WIP-1001 keyring registry.
//!
//! [`KeyringRegistry`] is the interface between the [signature
//! verifier](super::verify) and whatever holds the precompile-managed keyring
//! state — the pool's state provider, the builder, the RPC layer, or a test
//! double. It deliberately mirrors `IWorldIDKeyRing.isAuthorized` from the
//! spec so the eventual precompile-backed implementation is a thin adapter.
//!
//! [`validate_wip1001`] is the one-stop entry point that callers (pool,
//! builder, RPC) should use: it computes the signing hash, runs the per-scheme
//! cryptographic verification, and gates the result on the registry's
//! `is_authorized` answer.

use alloy_primitives::Address;

use crate::transaction::{
    SessionKey, TxWip1001, Wip1001Signature, Wip1001VerifyError, verify_wip1001_signature,
};

/// Read-only view of the precompile-managed keyring state.
///
/// Implementors translate `is_authorized` into a state lookup at the current
/// block height. The trait is sync because pool/builder admission paths are
/// sync; async callers can wrap a sync registry behind their own boundary.
pub trait KeyringRegistry {
    /// Backend-specific lookup error (e.g. database failure). Returning
    /// `Ok(false)` denotes "key is not authorized"; an `Err` denotes "the
    /// answer is not available" and callers should treat the transaction as
    /// pending or rejected accordingly.
    type Error;

    /// Returns `Ok(true)` iff `session_key` is currently in the authorized set
    /// of `keyring`.
    fn is_authorized(
        &self,
        keyring: Address,
        session_key: &SessionKey,
    ) -> Result<bool, Self::Error>;
}

/// Errors returned by [`validate_wip1001`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Wip1001ValidationError<E> {
    /// Cryptographic verification failed (signature mismatch, low-s rule,
    /// WebAuthn flag/challenge violation, …).
    #[error(transparent)]
    Verify(#[from] Wip1001VerifyError),
    /// The registry could not answer the authorization query (e.g. state
    /// lookup error). Callers should typically treat the transaction as
    /// rejected or pending.
    #[error("keyring registry lookup failed: {0}")]
    Registry(E),
    /// Signature is valid but the recovered session key is not in the
    /// keyring's authorized set at the current state height.
    #[error("session key is not authorized for keyring {keyring}")]
    NotAuthorized {
        /// The keyring address that the unauthorized key was presented for.
        keyring: Address,
    },
}

/// Verify a WIP-1001 transaction's signature against its embedded
/// `session_key`, then assert that the registry authorizes the key for the
/// declared keyring.
///
/// On success returns the typed [`SessionKey`] that was bound to the
/// signature, ready to be passed onward to gas/nonce accounting.
pub fn validate_wip1001<R: KeyringRegistry>(
    tx: &TxWip1001,
    sig: &Wip1001Signature,
    registry: &R,
) -> Result<SessionKey, Wip1001ValidationError<R::Error>> {
    let signing_hash = tx.signing_hash();
    let session_key = verify_wip1001_signature(
        tx.signature_type,
        tx.session_key.as_ref(),
        sig,
        &signing_hash,
    )?;

    match registry.is_authorized(tx.keyring, &session_key) {
        Ok(true) => Ok(session_key),
        Ok(false) => Err(Wip1001ValidationError::NotAuthorized {
            keyring: tx.keyring,
        }),
        Err(e) => Err(Wip1001ValidationError::Registry(e)),
    }
}

#[cfg(any(test, feature = "test-utils"))]
mod mock {
    use super::*;
    use std::{
        collections::{HashMap, HashSet},
        convert::Infallible,
    };

    /// In-memory [`KeyringRegistry`] for tests and downstream development.
    ///
    /// Authorizations are stored explicitly; lookups never fail (`Error =
    /// Infallible`). For richer scenarios (e.g. simulating a state-lookup
    /// failure) write a bespoke mock in the calling crate.
    #[derive(Debug, Default, Clone)]
    pub struct MockKeyringRegistry {
        authorized: HashMap<Address, HashSet<SessionKey>>,
    }

    impl MockKeyringRegistry {
        /// Creates an empty registry with no authorized session keys.
        pub fn new() -> Self {
            Self::default()
        }

        /// Authorizes `key` on `keyring`. Idempotent.
        pub fn authorize(&mut self, keyring: Address, key: SessionKey) {
            self.authorized.entry(keyring).or_default().insert(key);
        }

        /// Revokes `key` on `keyring`. Returns `true` if a removal occurred.
        pub fn revoke(&mut self, keyring: Address, key: &SessionKey) -> bool {
            self.authorized
                .get_mut(&keyring)
                .map(|set| set.remove(key))
                .unwrap_or(false)
        }
    }

    impl KeyringRegistry for MockKeyringRegistry {
        type Error = Infallible;

        fn is_authorized(
            &self,
            keyring: Address,
            session_key: &SessionKey,
        ) -> Result<bool, Self::Error> {
            Ok(self
                .authorized
                .get(&keyring)
                .is_some_and(|set| set.contains(session_key)))
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub use mock::MockKeyringRegistry;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::{P256Signature, signature::P256_PUBKEY_LEN, verify::P256N_HALF};
    use alloy_eips::eip2930::AccessList;
    use alloy_primitives::{B256, Bytes, U256, address, hex};
    use p256::{
        ecdsa::{SigningKey as P256SigningKey, signature::hazmat::PrehashSigner},
        elliptic_curve::rand_core::OsRng,
    };

    /// Lookup error stand-in for tests that need to simulate registry failures.
    #[derive(Debug, thiserror::Error, PartialEq, Eq, Clone, Copy)]
    #[error("registry unavailable")]
    struct LookupFailed;

    /// Registry that always errors. Useful for asserting we propagate
    /// `Registry(...)` instead of silently treating it as "not authorized".
    struct FailingRegistry;
    impl KeyringRegistry for FailingRegistry {
        type Error = LookupFailed;
        fn is_authorized(
            &self,
            _keyring: Address,
            _session_key: &SessionKey,
        ) -> Result<bool, Self::Error> {
            Err(LookupFailed)
        }
    }

    fn p256_keypair() -> (P256SigningKey, [u8; P256_PUBKEY_LEN]) {
        let sk = P256SigningKey::random(&mut OsRng);
        let vk = sk.verifying_key().to_encoded_point(false);
        let mut bytes = [0u8; P256_PUBKEY_LEN];
        bytes[..32].copy_from_slice(vk.x().expect("x").as_ref());
        bytes[32..].copy_from_slice(vk.y().expect("y").as_ref());
        (sk, bytes)
    }

    fn p256_sign(sk: &P256SigningKey, hash: &B256) -> P256Signature {
        let raw: p256::ecdsa::Signature = sk.sign_prehash(hash.as_slice()).expect("sign");
        let bytes = raw.to_bytes();
        let r = B256::from_slice(&bytes[..32]);
        let s_u = U256::from_be_slice(&bytes[32..]);
        let s_norm = if s_u > P256N_HALF {
            crate::transaction::verify::P256_ORDER - s_u
        } else {
            s_u
        };
        P256Signature {
            r,
            s: B256::from(s_norm.to_be_bytes::<32>()),
        }
    }

    /// Builds a P-256-signed `(tx, signature)` pair plus the typed session key.
    fn signed_p256() -> (TxWip1001, Wip1001Signature, SessionKey) {
        let (sk, key_bytes) = p256_keypair();
        let session_key = SessionKey::from_wire(Wip1001Signature::P256_TYPE, &key_bytes)
            .expect("typed session key");

        let tx = TxWip1001 {
            chain_id: 480,
            nonce: 0,
            max_priority_fee_per_gas: 1,
            max_fee_per_gas: 2,
            gas_limit: 21_000,
            to: address!("6069a6c32cf691f5982febae4faf8a6f3ab2f0f6").into(),
            value: U256::ZERO,
            input: hex!("").into(),
            access_list: AccessList::default(),
            keyring: address!("000000000000000000000000000000000000001d"),
            signature_type: Wip1001Signature::P256_TYPE,
            session_key: Bytes::copy_from_slice(&key_bytes),
        };
        let sig = Wip1001Signature::P256(p256_sign(&sk, &tx.signing_hash()));
        (tx, sig, session_key)
    }

    #[test]
    fn validate_ok_when_authorized() {
        let (tx, sig, key) = signed_p256();
        let mut registry = MockKeyringRegistry::new();
        registry.authorize(tx.keyring, key.clone());

        let recovered = validate_wip1001(&tx, &sig, &registry).expect("ok");
        assert_eq!(recovered, key);
    }

    #[test]
    fn validate_rejects_unauthorized_key() {
        let (tx, sig, _key) = signed_p256();
        // Empty registry — key is not authorized.
        let registry = MockKeyringRegistry::new();
        let err = validate_wip1001(&tx, &sig, &registry).expect_err("must reject");
        assert!(matches!(
            err,
            Wip1001ValidationError::NotAuthorized { keyring } if keyring == tx.keyring
        ));
    }

    #[test]
    fn validate_rejects_when_authorized_for_different_keyring() {
        let (tx, sig, key) = signed_p256();
        let mut registry = MockKeyringRegistry::new();
        // Authorize the key on a *different* keyring.
        registry.authorize(Address::with_last_byte(0xAA), key);
        let err = validate_wip1001(&tx, &sig, &registry).expect_err("must reject");
        assert!(matches!(
            err,
            Wip1001ValidationError::NotAuthorized { keyring } if keyring == tx.keyring
        ));
    }

    #[test]
    fn validate_propagates_registry_error() {
        let (tx, sig, _key) = signed_p256();
        let err = validate_wip1001(&tx, &sig, &FailingRegistry).expect_err("must propagate");
        assert!(matches!(
            err,
            Wip1001ValidationError::Registry(LookupFailed)
        ));
    }

    #[test]
    fn validate_rejects_tampered_signature_without_registry_call() {
        let (mut tx, sig, key) = signed_p256();
        // Mutate `input` so the cached signature no longer covers the message.
        tx.input = Bytes::from_static(b"tampered");

        // Registry that would PANIC if called — proves we error out before
        // touching it on a verification failure.
        struct PanicRegistry;
        impl KeyringRegistry for PanicRegistry {
            type Error = Infallible;
            fn is_authorized(&self, _: Address, _: &SessionKey) -> Result<bool, Infallible> {
                panic!("registry must not be consulted on verify failure");
            }
        }

        let err = validate_wip1001(&tx, &sig, &PanicRegistry).expect_err("verify must fail");
        assert!(matches!(err, Wip1001ValidationError::Verify(_)));
        // Silence unused-binding warnings on the original key.
        let _ = key;
    }

    #[test]
    fn mock_revoke_round_trip() {
        let (_, _, key) = signed_p256();
        let kr = address!("00000000000000000000000000000000000000aa");
        let mut registry = MockKeyringRegistry::new();
        registry.authorize(kr, key.clone());
        assert!(registry.is_authorized(kr, &key).unwrap());
        assert!(registry.revoke(kr, &key));
        assert!(!registry.is_authorized(kr, &key).unwrap());
        // Idempotent revoke.
        assert!(!registry.revoke(kr, &key));
    }

    use std::convert::Infallible;
}
