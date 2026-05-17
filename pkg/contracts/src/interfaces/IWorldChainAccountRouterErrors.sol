// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @title IWorldChainAccountRouterErrors
/// @author 0xOsiris, World Contributors
/// @notice Custom errors raised by the WIP-1001 account router.
/// @custom:security-contact security@toolsforhumanity.com
interface IWorldChainAccountRouterErrors {
    /// @notice Thrown when `installAdmin` is called more than once for this account.
    error AdminAlreadyInstalled();

    /// @notice Thrown when an admin-signature dispatch is requested before `installAdmin`.
    error AdminNotInstalled();

    /// @notice Thrown when an install path is called by an address other than
    ///         `WORLD_CHAIN_ACCOUNT_MANAGER`.
    error CallerNotManager();

    /// @notice Thrown when `installAdmin` is called with `admin.verifier == address(0)`.
    error ZeroAdminVerifier();

    /// @notice Thrown when a session dispatch references a verifier outside the active key ring.
    ///         Raised BEFORE any verifier code executes, satisfying the WIP-1001 requirement
    ///         that unknown session verifier addresses MUST fail without executing verifier
    ///         implementation code.
    error VerifierNotInstalled(address verifier);
}
