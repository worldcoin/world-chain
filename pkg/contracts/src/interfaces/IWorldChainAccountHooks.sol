// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @title IWorldChainAccountHooks
/// @author Worldcoin
/// @notice Installation hook called via `DELEGATECALL` from the account router so that a
///         verifier implementation can materialise its account-scoped state into the calling
///         account's storage.
/// @dev Implementations MUST namespace their storage with a slot root derived from the verifier
///      implementation's deployed address (captured into an `immutable` at construction so that
///      `address(this)` shadowing under `DELEGATECALL` does not affect the namespace). The
///      recommended pattern is:
///
///      ```solidity
///      address private immutable _SELF = address(this);
///      function _slot() private view returns (bytes32) {
///          return keccak256(abi.encode("WIP1001.VERIFIER.STORAGE", _SELF));
///      }
///      ```
///
///      Verifier authors are responsible for collision-freedom across installed verifiers.
/// @custom:security-contact security@toolsforhumanity.com
interface IWorldChainAccountHooks {
    /// @notice Provision verifier-scoped state from `installation` into the calling account's
    ///         storage namespace.
    /// @dev MUST be invoked only via `DELEGATECALL` from the account router. The protocol does
    ///      not parse `installation`; its layout is verifier-defined and is committed to by the
    ///      manager via `adminHash`, `createHash`, and `keyRingHash`.
    /// @param installation Opaque verifier-specific installation calldata.
    function install(bytes calldata installation) external;
}
