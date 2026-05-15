// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IWorldChainAccountManager} from "./IWorldChainAccountManager.sol";
import {IWorldChainSessionVerifier} from "./IWorldChainSessionVerifier.sol";

/// @title IWorldChainAccountRouter
/// @author Worldcoin
/// @notice Interface of the per-account router that dispatches admin and session validation
///         calls to the account's installed EIP-1271 verifier implementations.
/// @dev The router holds the only contract-level state that drives WIP-1001 validation
///      dispatch. `installAdmin` / `installKeyRing` are gated to the manager. The three
///      validation functions are callable by anyone, but the manager performs them inside
///      restricted validation frames and is the only consumer the protocol depends on.
/// @custom:security-contact security@toolsforhumanity.com
interface IWorldChainAccountRouter {
    /// @notice Installs the immutable admin verifier configuration. MUST be called exactly
    ///         once, only by the manager, during `IWorldChainAccountManager.create`.
    function installAdmin(IWorldChainAccountManager.WorldChainAccountVerifier calldata admin) external;

    /// @notice Replaces the full session-verifier set. MUST be called only by the manager.
    function installKeyRing(IWorldChainAccountManager.WorldChainAccountVerifier[] calldata sessionVerifiers) external;

    /// @notice Dispatches an EIP-1271 signature check to the installed admin verifier.
    /// @param hash The domain-separated admin operation hash.
    /// @param signature The admin authorization calldata.
    /// @return magicValue `0x1626ba7e` iff the admin verifier accepts.
    function isValidSignatureForAdmin(bytes32 hash, bytes calldata signature) external view returns (bytes4 magicValue);

    /// @notice Dispatches an EIP-1271 signature check to an installed session verifier.
    /// @dev Returns a non-magic value when `verifier` is not in the active session-verifier
    ///      set; verifier implementation code is never executed in that case.
    function isValidSignatureForVerifier(address verifier, bytes32 hash, bytes calldata signature)
        external
        view
        returns (bytes4 magicValue);

    /// @notice Dispatches a session-policy evaluation to an installed session verifier.
    /// @dev Returns `false` when `verifier` is not in the active session-verifier set;
    ///      verifier implementation code is never executed in that case.
    function evaluateSessionPolicyForVerifier(
        address verifier,
        IWorldChainSessionVerifier.ExecutionTraceContext calldata context
    ) external view returns (bool allowed);

    /// @notice The manager allowed to mutate this router's verifier set.
    function manager() external view returns (address);

    /// @notice The currently installed admin verifier address.
    function adminVerifier() external view returns (address);
}
