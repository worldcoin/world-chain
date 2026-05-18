//! WIP-1001 signature verification.
//!
//! Generalized verification defined in the
//! [WIP-1001](https://github.com/worldcoin/world-chain/blob/main/wips/wip-1001.md).

use crate::transaction::Wip1001Signature;
use alloy_consensus::crypto::RecoveryError;
use alloy_primitives::{Address, B256};

/// Read-only backend that performs the EIP-1271 signature validation call
/// described in WIP-1001's "Account Router and Verifier Interfaces" and
/// "Restricted Validation Frames" sections.
///
/// An implementation MUST, acting as `WORLD_CHAIN_ACCOUNT_MANAGER`, perform a
/// `STATICCALL` to `world_chain_account` with:
///
/// - calldata `IWorldChainAccountRouter.isValidSignatureForVerifier.selector
///   || abi.encode(session_verifier, signing_hash, signature)`,
/// - value `0`,
/// - gas exactly `EIP1271_VALIDATION_GAS_LIMIT`,
/// - the WIP-1001 restricted-frame tracer attached.
///
/// The account router at `world_chain_account` then dispatches the call to
/// `session_verifier`'s EIP-1271 entry via `DELEGATECALL`, so the verifier
/// implementation executes against the account's storage. The call succeeds
/// only if the router returns `EIP1271_MAGIC_VALUE` and no tracer rule is
/// violated.
///
/// Per WIP-1001, revert, out-of-gas, malformed return data, missing magic
/// value, and tracer rule violations are all treated identically as
/// "signature failure". Implementations are free to surface richer cause
/// information through [`SessionVerifier::Error`], but
/// [`verify_wip1001_signature`] collapses every failure mode into
/// [`Wip1001VerifyError::Invalid`].
pub trait SessionVerifier {
    /// Backend-specific error. A real EVM-backed implementation will surface
    /// state-lookup failures, EVM execution errors, tracer violations, etc.;
    /// a mock can use [`std::convert::Infallible`].
    type Error;

    /// Returns `Ok(())` iff the router-dispatched EIP-1271 call returns
    /// `EIP1271_MAGIC_VALUE` and triggers no tracer rule violation.
    ///
    /// `world_chain_account` is the `STATICCALL` target: the account
    /// address, whose deployed bytecode is the world chain account router.
    /// `session_verifier` is the dispatch identifier forwarded to the
    /// router's `isValidSignatureForVerifier`; it is **not** the
    /// `STATICCALL` target.
    fn is_valid_signature(
        &self,
        world_chain_account: Address,
        session_verifier: Address,
        signing_hash: B256,
        signature: &Wip1001Signature,
    ) -> Result<(), Self::Error>;
}

/// Errors returned by [`verify_wip1001_signature`].
///
/// Intentionally non-generic so it composes with the cross-module
/// validation error in [`crate::transaction::keyring`]. The protocol folds
/// every EIP-1271 failure mode (revert, OOG, malformed return, tracer
/// violation, missing magic value) into the single `Invalid` variant;
/// callers that need richer diagnostics should invoke
/// [`SessionVerifier::is_valid_signature`] directly and inspect
/// [`SessionVerifier::Error`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Wip1001VerifyError {
    /// The router-dispatched EIP-1271 call did not authorize the signature.
    #[error("WIP-1001 signature is not valid for the supplied session verifier")]
    Invalid,
}

impl From<Wip1001VerifyError> for RecoveryError {
    fn from(_: Wip1001VerifyError) -> Self {
        Self::new()
    }
}

/// Verify a WIP-1001 signature against `signing_hash` for the
/// `(world_chain_account, session_verifier)` pair, using the supplied
/// [`SessionVerifier`] backend.
///
/// The caller is responsible for separately invoking
/// `IWorldChainAccountManager.isAuthorizedSessionVerifier(world_chain_account, session_verifier)`
/// — see [`crate::transaction::keyring`].
pub fn verify_wip1001_signature<V: SessionVerifier>(
    verifier: &V,
    world_chain_account: Address,
    session_verifier: Address,
    signing_hash: B256,
    signature: &Wip1001Signature,
) -> Result<(), Wip1001VerifyError> {
    verifier
        .is_valid_signature(
            world_chain_account,
            session_verifier,
            signing_hash,
            signature,
        )
        .map_err(|_| Wip1001VerifyError::Invalid)
}
