// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Address} from "@openzeppelin/contracts/utils/Address.sol";

import {IWorldChainAccount} from "../interfaces/IWorldChainAccount.sol";
import {WorldChainAccountVerifier} from "../interfaces/IWorldChainAccountManager.sol";
import {IWorldChainAccountHooks} from "../interfaces/IWorldChainAccountHooks.sol";
import {IWorldChainAccountRouterErrors} from "../interfaces/IWorldChainAccountRouterErrors.sol";

/// @title KeyRingStore
/// @author 0xOsiris, World Contributors
/// @notice Account-scoped storage for the admin verifier, the canonical key-ring hash, and the
///         per-verifier installation marker. Encapsulates the WIP-1001 membership invariant:
///
///             v ∈ active key ring  ⇔  sessionKeyRing[v] == keyRingHash ≠ 0
///
///         and exposes installation hooks that DELEGATECALL into each verifier's
///         `IWorldChainAccountHooks.install`. Storage lives under an ERC-7201 namespaced slot so
///         verifier-installed state cannot collide with router-owned state. The
///         `IWorldChainAccount.KeyRingStorage` layout is declared on the public interface so
///         off-chain consumers can introspect the slot layout from the ABI.
/// @custom:security-contact security@toolsforhumanity.com
abstract contract KeyRingStore {
    using Address for address;

    /// @dev `keccak256(abi.encode(uint256(keccak256("worldchain.account.keyring")) - 1)) & ~bytes32(uint256(0xff))`
    bytes32 private constant KEY_RING_STORAGE_LOCATION =
        0xb754a00ef4e1c0ba493e01b1f93db435f9393d01bd0f7554a936d77311120300;

    /// @notice Requires `verifier` to be a member of the active key ring. Reverts BEFORE any
    ///         verifier code executes, satisfying the WIP-1001 requirement that unknown session
    ///         verifier addresses MUST fail without executing verifier implementation code.
    modifier sessionInstalled(address verifier) {
        IWorldChainAccount.KeyRingStorage storage $ = _keyRingStorage();
        bytes32 hash = $.keyRingHash;
        if (hash == bytes32(0) || $.sessionKeyRing[verifier] != hash) {
            revert IWorldChainAccountRouterErrors.VerifierNotInstalled(verifier);
        }
        _;
    }

    /// @notice Returns the installed admin verifier address, or `address(0)` if `_installAdmin`
    ///         has not been called.
    function _adminVerifier() internal view returns (address) {
        return _keyRingStorage().adminVerifier;
    }

    /// @notice Returns the canonical `keyRingHash` of the current session verifier set.
    function _keyringHash() internal view returns (bytes32) {
        return _keyRingStorage().keyRingHash;
    }

    /// @notice Returns the per-verifier installation marker for `verifier`.
    function _sessionKeyRing(address verifier) internal view returns (bytes32) {
        return _keyRingStorage().sessionKeyRing[verifier];
    }

    /// @notice Installs the immutable admin verifier and runs its installation hook. Reverts if an
    ///         admin is already installed or if the verifier address is zero.
    function _installAdmin(WorldChainAccountVerifier calldata admin) internal {
        if (admin.verifier == address(0)) revert IWorldChainAccountRouterErrors.ZeroAdminVerifier();
        IWorldChainAccount.KeyRingStorage storage $ = _keyRingStorage();
        if ($.adminVerifier != address(0)) revert IWorldChainAccountRouterErrors.AdminAlreadyInstalled();
        $.adminVerifier = admin.verifier;
        _runInstallHook(admin.verifier, admin.installation);
    }

    /// @notice Replaces the active key ring with `sessionVerifiers`, rotating the canonical hash
    ///         and running each verifier's installation hook. The hash rotation atomically retires
    ///         every entry from the prior key ring without an explicit per-element wipe.
    function _installKeyRing(WorldChainAccountVerifier[] calldata sessionVerifiers) internal {
        IWorldChainAccount.KeyRingStorage storage $ = _keyRingStorage();
        bytes32 hash = keccak256(abi.encode(sessionVerifiers));
        $.keyRingHash = hash;
        uint256 n = sessionVerifiers.length;
        for (uint256 i; i < n; ++i) {
            WorldChainAccountVerifier calldata v = sessionVerifiers[i];
            $.sessionKeyRing[v.verifier] = hash;
            _runInstallHook(v.verifier, v.installation);
        }
    }

    /// @dev DELEGATECALLs `IWorldChainAccountHooks.install(installation)` on `verifier` so the
    ///      verifier writes any validation-affecting state into its own deterministic account
    ///      storage namespace. Uses OpenZeppelin's `Address.functionDelegateCall`, which reverts
    ///      with the callee's returndata on failure.
    function _runInstallHook(address verifier, bytes calldata installation) private {
        verifier.functionDelegateCall(abi.encodeCall(IWorldChainAccountHooks.install, (installation)));
    }

    function _keyRingStorage() private pure returns (IWorldChainAccount.KeyRingStorage storage $) {
        assembly ("memory-safe") {
            $.slot := KEY_RING_STORAGE_LOCATION
        }
    }
}
