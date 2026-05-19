//! Authorization lookup against the WIP-1001 keyring registry.
//!
//! [`WorldChainAccountManager`] is the interface between the [signature
//! verifier](super::verify) and whatever holds the predeploy-managed keyring
//! state — the pool's state provider, the builder, the RPC layer, or a test
//! double. It deliberately mirrors `IWorldChainAccountManager.isAuthorizedSessionVerifier`
//! from the spec so the eventual predeploy-backed implementation is a thin adapter.
//!
//! [`validate_wip1001`] is the one-stop entry point that callers (pool,
//! builder, RPC) should use: it computes the signing hash, runs the per-scheme
//! cryptographic verification, and gates the result on the registry's
//! `isAuthorizedSessionVerifier` answer.

use crate::transaction::{
    TxWip1001, Wip1001Signature,
    verify::{SessionVerifier, Wip1001VerifyError, verify_wip1001_signature},
};
use alloy_primitives::Address;

/// Read-only view of the predeploy-managed world chain account manager.
///
/// Implementors translate `isAuthorizedSessionVerifier` into a state lookup at the
/// current block height. The trait is sync because pool/builder admission paths are
/// sync; async callers can wrap a sync registry behind their own boundary.
pub trait WorldChainAccountManager {
    /// Backend-specific lookup error (e.g. database failure). Returning
    /// `Ok(false)` denotes "key is not authorized"; an `Err` denotes "the
    /// answer is not available" and callers should treat the transaction as
    /// pending or rejected accordingly.
    type Error;

    /// Returns `Ok(true)` iff `sessionVerifier` is currently in the authorized set
    /// of `world_chain_account`.
    fn is_authorized_session_verifier(
        &self,
        world_chain_account: Address,
        session_verifier: Address,
    ) -> Result<bool, Self::Error>;

    /// Returns the current `Account.transactionNonce` for `world_chain_account`
    /// as exposed by `IWorldChainAccountManager.getTransactionNonce`.
    ///
    /// Per WIP-1001 §"Transaction Type 0x1D", a `0x1D` envelope's `nonce` MUST
    /// equal this value at execution time. Pool-admission callers SHOULD reject
    /// only strictly smaller envelope nonces (stale) and allow `>=` so future
    /// nonces can park as queued, mirroring standard EOA nonce ordering.
    ///
    /// Returns `0` for an address that does not have a managed account (the
    /// mapping default). Callers that need to distinguish "no account" from
    /// "account with nonce 0" MUST anchor on a separate existence check.
    fn get_transaction_nonce(&self, world_chain_account: Address) -> Result<u64, Self::Error>;
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
    /// Signature is valid but the session verifier is not in the
    /// world chain account's authorized set at the current state height.
    #[error("session key is not authorized for world chain account {world_chain_account}")]
    NotAuthorized {
        /// The world chain account address that the unauthorized key was presented for.
        world_chain_account: Address,
    },
}

/// Verify a WIP-1001 transaction's signature against its embedded
/// `session_verifier`, then assert that the registry authorizes the key for the
/// declared world chain account.
pub fn validate_wip1001<M, V>(
    tx: &TxWip1001,
    sig: &Wip1001Signature,
    world_chain_account_manager: &M,
    session_verifier: &V,
) -> Result<(), Wip1001ValidationError<M::Error>>
where
    M: WorldChainAccountManager,
    V: SessionVerifier,
{
    let world_chain_account_addr = tx.world_chain_account;
    let session_verifier_addr = tx.session_verifier;
    match world_chain_account_manager
        .is_authorized_session_verifier(world_chain_account_addr, session_verifier_addr)
    {
        Ok(true) => {
            let signing_hash = tx.signing_hash();
            verify_wip1001_signature(
                session_verifier,
                world_chain_account_addr,
                session_verifier_addr,
                signing_hash,
                sig,
            )
            .map_err(|_| Wip1001VerifyError::Invalid)?;
            Ok(())
        }
        Ok(false) => Err(Wip1001ValidationError::NotAuthorized {
            world_chain_account: world_chain_account_addr,
        }),
        Err(e) => Err(Wip1001ValidationError::Registry(e)),
    }
}

#[cfg(any(test, feature = "test-utils"))]
mod mock {
    use super::*;
    use alloy_primitives::B256;
    use std::{
        collections::{HashMap, HashSet},
        convert::Infallible,
    };

    /// In-memory [`WorldChainAccountManager`] for tests and downstream development.
    ///
    /// Authorizations are stored explicitly as `(world_chain_account ->
    /// {session_verifier_addresses})`; lookups never fail (`Error =
    /// Infallible`). For richer scenarios (e.g. simulating a state-lookup
    /// failure) write a bespoke mock in the calling crate.
    #[derive(Debug, Default, Clone)]
    pub struct MockKeyringRegistry {
        authorized: HashMap<Address, HashSet<Address>>,
        transaction_nonces: HashMap<Address, u64>,
    }

    impl MockKeyringRegistry {
        /// Creates an empty registry with no authorized session verifiers.
        pub fn new() -> Self {
            Self::default()
        }

        /// Authorizes `session_verifier` on `world_chain_account`. Idempotent.
        pub fn authorize(&mut self, world_chain_account: Address, session_verifier: Address) {
            self.authorized
                .entry(world_chain_account)
                .or_default()
                .insert(session_verifier);
        }

        /// Revokes `session_verifier` on `world_chain_account`. Returns `true`
        /// if a removal occurred.
        pub fn revoke(&mut self, world_chain_account: Address, session_verifier: &Address) -> bool {
            self.authorized
                .get_mut(&world_chain_account)
                .map(|set| set.remove(session_verifier))
                .unwrap_or(false)
        }

        /// Sets the `transactionNonce` reported for `world_chain_account`.
        /// Unset accounts default to `0`, mirroring the predeploy mapping.
        pub fn set_transaction_nonce(&mut self, world_chain_account: Address, nonce: u64) {
            self.transaction_nonces.insert(world_chain_account, nonce);
        }
    }

    impl WorldChainAccountManager for MockKeyringRegistry {
        type Error = Infallible;

        fn is_authorized_session_verifier(
            &self,
            world_chain_account: Address,
            session_verifier: Address,
        ) -> Result<bool, Self::Error> {
            Ok(self
                .authorized
                .get(&world_chain_account)
                .is_some_and(|set| set.contains(&session_verifier)))
        }

        fn get_transaction_nonce(
            &self,
            world_chain_account: Address,
        ) -> Result<u64, Self::Error> {
            Ok(self
                .transaction_nonces
                .get(&world_chain_account)
                .copied()
                .unwrap_or(0))
        }
    }

    /// In-memory [`SessionVerifier`] for tests and downstream development.
    ///
    /// Returns a fixed verdict (`accept` or reject) for every call; the
    /// real backend is an EVM-driven STATICCALL through the account router
    /// (see [`SessionVerifier`] and WIP-1001 §"Restricted Validation Frames").
    #[derive(Debug, Clone, Copy, Default)]
    pub struct MockSessionVerifier {
        accept: bool,
    }

    impl MockSessionVerifier {
        /// Returns a verifier that accepts every signature.
        pub fn accept_all() -> Self {
            Self { accept: true }
        }

        /// Returns a verifier that rejects every signature.
        pub fn reject_all() -> Self {
            Self { accept: false }
        }
    }

    /// Error returned by [`MockSessionVerifier::reject_all`].
    #[derive(Debug, thiserror::Error, PartialEq, Eq, Clone, Copy)]
    #[error("mock session verifier rejected the signature")]
    pub struct MockSessionVerifierRejected;

    impl SessionVerifier for MockSessionVerifier {
        type Error = MockSessionVerifierRejected;

        fn is_valid_signature(
            &self,
            _world_chain_account: Address,
            _session_verifier: Address,
            _signing_hash: B256,
            _signature: &Wip1001Signature,
        ) -> Result<(), Self::Error> {
            if self.accept {
                Ok(())
            } else {
                Err(MockSessionVerifierRejected)
            }
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub use mock::{MockKeyringRegistry, MockSessionVerifier, MockSessionVerifierRejected};

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_eips::eip2930::AccessList;
    use alloy_primitives::{Bytes, U256, address, hex};
    use std::convert::Infallible;

    /// Lookup error stand-in for tests that need to simulate registry failures.
    #[derive(Debug, thiserror::Error, PartialEq, Eq, Clone, Copy)]
    #[error("registry unavailable")]
    struct LookupFailed;

    /// Registry that always errors. Useful for asserting we propagate
    /// `Registry(...)` instead of silently treating it as "not authorized".
    struct FailingRegistry;
    impl WorldChainAccountManager for FailingRegistry {
        type Error = LookupFailed;
        fn is_authorized_session_verifier(
            &self,
            _world_chain_account: Address,
            _session_verifier: Address,
        ) -> Result<bool, Self::Error> {
            Err(LookupFailed)
        }
        fn get_transaction_nonce(
            &self,
            _world_chain_account: Address,
        ) -> Result<u64, Self::Error> {
            Err(LookupFailed)
        }
    }

    /// Builds a fixed `(tx, signature)` pair. The signature payload is opaque
    /// to the protocol (verification is delegated to the on-chain session
    /// verifier via EIP-1271) so we use a constant blob.
    fn signed_tx() -> (TxWip1001, Wip1001Signature) {
        let tx = TxWip1001 {
            chain_id: 480,
            nonce: 0,
            max_priority_fee_per_gas: 1,
            max_fee_per_gas: 2,
            gas_limit: 21_000,
            world_chain_account: address!("000000000000000000000000000000000000001d"),
            session_verifier: address!("00000000000000000000000000000000000000aa"),
            to: address!("6069a6c32cf691f5982febae4faf8a6f3ab2f0f6").into(),
            value: U256::ZERO,
            input: Bytes::default(),
            access_list: AccessList::default(),
        };
        let sig = Wip1001Signature {
            signature: hex!("deadbeefcafef00d").into(),
        };
        (tx, sig)
    }

    #[test]
    fn validate_ok_when_authorized_and_session_verifier_accepts() {
        let (tx, sig) = signed_tx();
        let mut registry = MockKeyringRegistry::new();
        registry.authorize(tx.world_chain_account, tx.session_verifier);

        validate_wip1001(&tx, &sig, &registry, &MockSessionVerifier::accept_all()).expect("ok");
    }

    #[test]
    fn validate_rejects_unauthorized_verifier() {
        let (tx, sig) = signed_tx();
        // Empty registry — session verifier is not authorized.
        let registry = MockKeyringRegistry::new();

        let err = validate_wip1001(&tx, &sig, &registry, &MockSessionVerifier::accept_all())
            .expect_err("must reject");
        assert!(matches!(
            err,
            Wip1001ValidationError::NotAuthorized { world_chain_account }
                if world_chain_account == tx.world_chain_account
        ));
    }

    #[test]
    fn validate_rejects_when_authorized_for_different_account() {
        let (tx, sig) = signed_tx();
        let mut registry = MockKeyringRegistry::new();
        // Authorize the verifier on a *different* world chain account.
        registry.authorize(Address::with_last_byte(0xAA), tx.session_verifier);

        let err = validate_wip1001(&tx, &sig, &registry, &MockSessionVerifier::accept_all())
            .expect_err("must reject");
        assert!(matches!(
            err,
            Wip1001ValidationError::NotAuthorized { world_chain_account }
                if world_chain_account == tx.world_chain_account
        ));
    }

    #[test]
    fn validate_propagates_registry_error() {
        let (tx, sig) = signed_tx();
        let err = validate_wip1001(
            &tx,
            &sig,
            &FailingRegistry,
            &MockSessionVerifier::accept_all(),
        )
        .expect_err("must propagate");
        assert!(matches!(
            err,
            Wip1001ValidationError::Registry(LookupFailed)
        ));
    }

    #[test]
    fn validate_returns_verify_when_session_verifier_rejects() {
        let (tx, sig) = signed_tx();
        let mut registry = MockKeyringRegistry::new();
        registry.authorize(tx.world_chain_account, tx.session_verifier);

        let err = validate_wip1001(&tx, &sig, &registry, &MockSessionVerifier::reject_all())
            .expect_err("must reject signature");
        assert!(matches!(err, Wip1001ValidationError::Verify(_)));
    }

    #[test]
    fn validate_skips_session_verifier_when_unauthorized() {
        // Asserts the spec-mandated ordering: the authorization precheck
        // (WIP-1001 step 3) runs *before* the EIP-1271 signature call (step 6).
        let (tx, sig) = signed_tx();
        let registry = MockKeyringRegistry::new();

        struct PanicVerifier;
        impl SessionVerifier for PanicVerifier {
            type Error = Infallible;
            fn is_valid_signature(
                &self,
                _: Address,
                _: Address,
                _: alloy_primitives::B256,
                _: &Wip1001Signature,
            ) -> Result<(), Self::Error> {
                panic!("session verifier must not be consulted on unauthorized verifier");
            }
        }

        let err = validate_wip1001(&tx, &sig, &registry, &PanicVerifier)
            .expect_err("authorization precheck must fail first");
        assert!(matches!(
            err,
            Wip1001ValidationError::NotAuthorized { world_chain_account }
                if world_chain_account == tx.world_chain_account
        ));
    }

    #[test]
    fn mock_transaction_nonce_default_and_override() {
        let account = address!("00000000000000000000000000000000000000aa");
        let mut registry = MockKeyringRegistry::new();

        // Unset accounts default to 0 (matches the predeploy mapping default).
        assert_eq!(registry.get_transaction_nonce(account).unwrap(), 0);

        registry.set_transaction_nonce(account, 7);
        assert_eq!(registry.get_transaction_nonce(account).unwrap(), 7);

        // Unrelated account remains at 0.
        let other = address!("00000000000000000000000000000000000000bb");
        assert_eq!(registry.get_transaction_nonce(other).unwrap(), 0);
    }

    #[test]
    fn mock_authorize_and_revoke_round_trip() {
        let account = address!("00000000000000000000000000000000000000aa");
        let session_verifier = address!("00000000000000000000000000000000000000bb");
        let mut registry = MockKeyringRegistry::new();

        registry.authorize(account, session_verifier);
        assert!(
            registry
                .is_authorized_session_verifier(account, session_verifier)
                .unwrap()
        );

        assert!(registry.revoke(account, &session_verifier));
        assert!(
            !registry
                .is_authorized_session_verifier(account, session_verifier)
                .unwrap()
        );

        // Idempotent revoke.
        assert!(!registry.revoke(account, &session_verifier));
    }
}
