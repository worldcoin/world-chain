// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @title IWorldChainAccountHooks
/// @author 0xOsiris, World Contributors
/// @notice Installation hook through which verifier implementations install scoped storage. A
///         verifier implementation's `install` function MUST fully write all validation-affecting
///         state implied by `installation` into the verifier's own deterministic account-storage
///         namespace.
/// @custom:security-contact security@toolsforhumanity.com
interface IWorldChainAccountHooks {
    /// @notice Installs the verifier's account-scoped storage. Called by the account router during
    ///         `installAdmin` and `installKeyRing` flows in a single controlled `DELEGATECALL`,
    ///         so the implementation executes against the account's storage.
    /// @dev Verifier implementations MUST NOT rely on validation-affecting state stored at the
    ///      verifier implementation address. All validation-affecting state implied by
    ///      `installation` MUST be written into the verifier's own deterministic account-storage
    ///      namespace.
    /// @param installation Verifier-defined installation payload. Opaque to the protocol.
    function install(bytes calldata installation) external;
}
