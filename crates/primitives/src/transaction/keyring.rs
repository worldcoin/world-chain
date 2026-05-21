//! Authorization lookup against the WIP-1001 keyring registry.
//!
//! [`WorldChainAccountManager`] deliberately mirrors
//! `IWorldChainAccountManager.isAuthorizedSessionVerifier` from the spec
//! so the eventual predeploy-backed implementation is a thin adapter.
//!
//! [`WorldChainAccountRouter`] mirrors
//! `IWorldChainAccountRouter.isValidSignatureForVerifier`, which dispatches an
//! EIP-1271 call against the configured session verifier instance and returns
//! the EIP-1271 magic value ([`MAGIC_VALUE`]) on success.
//!
//! [`validate_wip1001`] is the one-stop entry point that callers (pool,
//! builder, RPC) should use: it gates on the registry's
//! `isAuthorizedSessionVerifier` answer first, then dispatches the EIP-1271
//! signature check via the account router.

use crate::transaction::{TxWip1001, Wip1001Signature};
use alloy_primitives::{Address, B256, Bytes, FixedBytes, hex};

/// EIP-1271 magic value (`0x1626ba7e`) returned by the account router when a
/// signature check succeeds.
pub const MAGIC_VALUE: FixedBytes<4> = FixedBytes::<4>::new(hex!("1626ba7e"));

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

    /// Returns `Ok(true)` only when `sessionVerifierInstance` resolves to an
    /// `InstalledSessionVerifier` whose `keyRingHash` equals the account's
    /// current `keyRingHash`.
    fn is_authorized_session_verifier(
        &self,
        world_chain_account: Address,
        session_verifier_instance: B256,
    ) -> Result<bool, Self::Error>;

    /// Returns the current `Account.transactionNonce` for `world_chain_account`
    /// as exposed by `IWorldChainAccountManager.getTransactionNonce`.
    ///
    /// Returns `0` for an address that does not have a managed account (the
    /// mapping default). Callers that need to distinguish "no account" from
    /// "account with nonce 0" MUST anchor on a separate existence check.
    fn get_transaction_nonce(&self, world_chain_account: Address) -> Result<u64, Self::Error>;
}

/// Read-only view of the predeploy-managed account router.
pub trait WorldChainAccountRouter {
    /// Backend-specific lookup error (e.g. database/EVM failure). An `Err`
    /// denotes "the answer is not available" and callers should typically
    /// treat the transaction as rejected.
    type Error;

    /// Dispatches an EIP-1271 signature check against the configured session
    /// verifier instance. Implementations MUST return the EIP-1271 magic value
    /// ([`MAGIC_VALUE`]) when the verifier accepts the signature, and any
    /// other 4-byte value otherwise.
    fn is_valid_signature_for_verifier(
        &self,
        session_verifier_instance: B256,
        hash: B256,
        signature: &Bytes,
    ) -> Result<FixedBytes<4>, Self::Error>;
}

/// Errors returned by [`validate_wip1001`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Wip1001ValidationError<E> {
    /// The router-dispatched EIP-1271 call could not be performed (e.g.
    /// underlying EVM/state error). Callers should typically treat the
    /// transaction as rejected or pending.
    #[error("contract interaction failed")]
    ContractInteraction,
    /// The router-dispatched EIP-1271 call did not return the magic value.
    #[error("WIP-1001 signature is not valid for the supplied session verifier instance")]
    Invalid,
    /// The registry could not answer the authorization query (e.g. state
    /// lookup error). Callers should typically treat the transaction as
    /// rejected or pending.
    #[error("keyring registry lookup failed: {0}")]
    Registry(E),
    /// Signature is valid but the session verifier instance is not in the
    /// world chain account's authorized set at the current state height.
    #[error("session key is not authorized for world chain account {world_chain_account}")]
    NotAuthorized {
        /// The world chain account address that the unauthorized key was presented for.
        world_chain_account: Address,
    },
}

/// Verify a WIP-1001 transaction's signature against its embedded
/// `session_verifier_instance`, then assert that the registry authorizes the
/// instance for the declared world chain account.
pub fn validate_wip1001<M, R>(
    tx: &TxWip1001,
    sig: &Wip1001Signature,
    world_chain_account_manager: &M,
    world_chain_account_router: &R,
) -> Result<(), Wip1001ValidationError<M::Error>>
where
    M: WorldChainAccountManager,
    R: WorldChainAccountRouter,
{
    let world_chain_account_addr = tx.world_chain_account;
    let session_verifier_instance = tx.session_verifier_instance;
    match world_chain_account_manager
        .is_authorized_session_verifier(world_chain_account_addr, session_verifier_instance)
    {
        Ok(true) => {
            let signing_hash = tx.signing_hash();
            let magic_bytes = world_chain_account_router
                .is_valid_signature_for_verifier(
                    session_verifier_instance,
                    signing_hash,
                    &sig.signature,
                )
                .map_err(|_| Wip1001ValidationError::ContractInteraction)?;
            if magic_bytes != MAGIC_VALUE {
                return Err(Wip1001ValidationError::Invalid);
            }
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
    /// {session_verifier_instance})`; lookups never fail (`Error =
    /// Infallible`). For richer scenarios (e.g. simulating a state-lookup
    /// failure) write a bespoke mock in the calling crate.
    #[derive(Debug, Default, Clone)]
    pub struct MockKeyringRegistry {
        authorized: HashMap<Address, HashSet<B256>>,
        transaction_nonces: HashMap<Address, u64>,
    }

    impl MockKeyringRegistry {
        /// Creates an empty registry with no authorized session verifier instances.
        pub fn new() -> Self {
            Self::default()
        }

        /// Authorizes `session_verifier_instance` on `world_chain_account`. Idempotent.
        pub fn authorize(&mut self, world_chain_account: Address, session_verifier_instance: B256) {
            self.authorized
                .entry(world_chain_account)
                .or_default()
                .insert(session_verifier_instance);
        }

        /// Revokes `session_verifier_instance` on `world_chain_account`. Returns
        /// `true` if a removal occurred.
        pub fn revoke(
            &mut self,
            world_chain_account: Address,
            session_verifier_instance: &B256,
        ) -> bool {
            self.authorized
                .get_mut(&world_chain_account)
                .map(|set| set.remove(session_verifier_instance))
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
            session_verifier_instance: B256,
        ) -> Result<bool, Self::Error> {
            Ok(self
                .authorized
                .get(&world_chain_account)
                .is_some_and(|set| set.contains(&session_verifier_instance)))
        }

        fn get_transaction_nonce(&self, world_chain_account: Address) -> Result<u64, Self::Error> {
            Ok(self
                .transaction_nonces
                .get(&world_chain_account)
                .copied()
                .unwrap_or(0))
        }
    }

    /// In-memory [`WorldChainAccountRouter`] for tests and downstream development.
    ///
    /// Returns a fixed verdict (`accept` returns [`MAGIC_VALUE`], `reject`
    /// returns zero bytes) for every call; the real backend is an EVM-driven
    /// DELEGATECALL through the account router into the installed session
    /// verifier implementation (see WIP-1001 §"Account Router and Verifier
    /// Interfaces" and §"Restricted Validation Frames").
    #[derive(Debug, Clone, Copy, Default)]
    pub struct MockWorldChainAccountRouter {
        accept: bool,
    }

    impl MockWorldChainAccountRouter {
        /// Returns a router that accepts every signature.
        pub fn accept_all() -> Self {
            Self { accept: true }
        }

        /// Returns a router that rejects every signature.
        pub fn reject_all() -> Self {
            Self { accept: false }
        }
    }

    impl WorldChainAccountRouter for MockWorldChainAccountRouter {
        type Error = Infallible;

        fn is_valid_signature_for_verifier(
            &self,
            _session_verifier_instance: B256,
            _hash: B256,
            _signature: &Bytes,
        ) -> Result<FixedBytes<4>, Self::Error> {
            if self.accept {
                Ok(MAGIC_VALUE)
            } else {
                Ok(FixedBytes::<4>::ZERO)
            }
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub use mock::{MockKeyringRegistry, MockWorldChainAccountRouter};

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_eips::eip2930::AccessList;
    use alloy_primitives::{Bytes, U256, address, hex};

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
            _session_verifier_instance: B256,
        ) -> Result<bool, Self::Error> {
            Err(LookupFailed)
        }
        fn get_transaction_nonce(&self, _world_chain_account: Address) -> Result<u64, Self::Error> {
            Err(LookupFailed)
        }
    }

    /// Router that always errors. Used to assert we surface `ContractInteraction`
    /// when the on-chain EIP-1271 dispatch can't be evaluated.
    #[derive(Debug, thiserror::Error, PartialEq, Eq, Clone, Copy)]
    #[error("router unavailable")]
    struct RouterFailed;

    struct FailingRouter;
    impl WorldChainAccountRouter for FailingRouter {
        type Error = RouterFailed;
        fn is_valid_signature_for_verifier(
            &self,
            _session_verifier_instance: B256,
            _hash: B256,
            _signature: &Bytes,
        ) -> Result<FixedBytes<4>, Self::Error> {
            Err(RouterFailed)
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
            session_verifier_instance: hex!(
                "00000000000000000000000000000000000000aa111111111111111111111111"
            )
            .into(),
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
    fn validate_ok_when_authorized_and_router_accepts() {
        let (tx, sig) = signed_tx();
        let mut registry = MockKeyringRegistry::new();
        registry.authorize(tx.world_chain_account, tx.session_verifier_instance);

        validate_wip1001(
            &tx,
            &sig,
            &registry,
            &MockWorldChainAccountRouter::accept_all(),
        )
        .expect("ok");
    }

    #[test]
    fn validate_rejects_unauthorized_verifier() {
        let (tx, sig) = signed_tx();
        // Empty registry — session verifier instance is not authorized.
        let registry = MockKeyringRegistry::new();

        let err = validate_wip1001(
            &tx,
            &sig,
            &registry,
            &MockWorldChainAccountRouter::accept_all(),
        )
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
        // Authorize the instance on a *different* world chain account.
        registry.authorize(Address::with_last_byte(0xAA), tx.session_verifier_instance);

        let err = validate_wip1001(
            &tx,
            &sig,
            &registry,
            &MockWorldChainAccountRouter::accept_all(),
        )
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
            &MockWorldChainAccountRouter::accept_all(),
        )
        .expect_err("must propagate");
        assert!(matches!(
            err,
            Wip1001ValidationError::Registry(LookupFailed)
        ));
    }

    #[test]
    fn validate_returns_invalid_when_router_rejects() {
        let (tx, sig) = signed_tx();
        let mut registry = MockKeyringRegistry::new();
        registry.authorize(tx.world_chain_account, tx.session_verifier_instance);

        let err = validate_wip1001(
            &tx,
            &sig,
            &registry,
            &MockWorldChainAccountRouter::reject_all(),
        )
        .expect_err("must reject signature");
        assert!(matches!(err, Wip1001ValidationError::Invalid));
    }

    #[test]
    fn validate_returns_contract_interaction_when_router_errors() {
        let (tx, sig) = signed_tx();
        let mut registry = MockKeyringRegistry::new();
        registry.authorize(tx.world_chain_account, tx.session_verifier_instance);

        let err = validate_wip1001(&tx, &sig, &registry, &FailingRouter)
            .expect_err("router errors must map to ContractInteraction");
        assert!(matches!(err, Wip1001ValidationError::ContractInteraction));
    }

    #[test]
    fn validate_skips_router_when_unauthorized() {
        // Asserts the spec-mandated ordering: the authorization precheck
        // (WIP-1001 step 3) runs *before* the EIP-1271 signature call (step 6).
        let (tx, sig) = signed_tx();
        let registry = MockKeyringRegistry::new();

        struct PanicRouter;
        impl WorldChainAccountRouter for PanicRouter {
            type Error = std::convert::Infallible;
            fn is_valid_signature_for_verifier(
                &self,
                _session_verifier_instance: B256,
                _hash: B256,
                _signature: &Bytes,
            ) -> Result<FixedBytes<4>, Self::Error> {
                panic!("router must not be consulted on unauthorized verifier");
            }
        }

        let err = validate_wip1001(&tx, &sig, &registry, &PanicRouter)
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
        let instance: B256 =
            hex!("00000000000000000000000000000000000000bb111111111111111111111111").into();
        let mut registry = MockKeyringRegistry::new();

        registry.authorize(account, instance);
        assert!(
            registry
                .is_authorized_session_verifier(account, instance)
                .unwrap()
        );

        assert!(registry.revoke(account, &instance));
        assert!(
            !registry
                .is_authorized_session_verifier(account, instance)
                .unwrap()
        );

        // Idempotent revoke.
        assert!(!registry.revoke(account, &instance));
    }
}
