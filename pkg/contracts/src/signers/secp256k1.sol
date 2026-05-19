// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IERC1271} from "@openzeppelin/contracts/interfaces/IERC1271.sol";
import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {IWorldChainAccountHooks} from "../interfaces/IWorldChainAccountHooks.sol";

/// @title WorldChainAdminVerifier
/// @author 0xOsiris, World Contributors
/// @notice EIP-1271 Verifier implementation for a World Chain account, authenticating one immutable secp256k1 EOA signer.
/// @custom:security-contact security@toolsforhumanity.com
contract SekP256K1Signer is IERC1271, IWorldChainAccountHooks {
    /// @notice Thrown when `install` is called with a zero signer.
    error ZeroSigner();

    /// @notice Thrown when `install` is called after the signer has already been set.
    error AlreadyInstalled();

    /// @notice Thrown when `install` is called with a payload that is not a single ABI-encoded
    ///         `address`.
    error InvalidInstallation();

    /// @notice Thrown when a signature is invalid according to the installed signer.
    error InvalidSignature(string reason);

    /// @dev Namespaced storage slot for the installed secp256k1 signer. Sized to a single word
    ///      (`address signer`) and unique to this verifier implementation, so it cannot collide
    ///      with the router's own state or with any other verifier installed against the account.
    bytes32 private constant SIGNER_SLOT = keccak256("worldchain.admin.verifier.secp256k1.v1.signer");

    /// @inheritdoc IWorldChainAccountHooks
    /// @dev `installation` MUST be `abi.encode(address signer)` with `signer != address(0)`.
    function install(bytes calldata installation) external {
        if (installation.length != 32) revert InvalidInstallation();
        address signer = abi.decode(installation, (address));
        if (signer == address(0)) revert ZeroSigner();

        bytes32 slot = SIGNER_SLOT;
        address current;
        assembly ("memory-safe") {
            current := sload(slot)
        }
        if (current != address(0)) revert AlreadyInstalled();

        assembly ("memory-safe") {
            sstore(slot, signer)
        }
    }

    /// @inheritdoc IERC1271
    /// @dev Returns `IERC1271.isValidSignature.selector` iff `signature` recovers to the installed
    ///      signer. `hash` MUST already be domain-separated by the calling protocol (WIP-1001);
    ///      no EIP-191 prefix is applied here.
    function isValidSignature(bytes32 hash, bytes memory signature) external view returns (bytes4 magicValue) {
        bytes32 slot = SIGNER_SLOT;
        address signer;
        assembly ("memory-safe") {
            signer := sload(slot)
        }
        if (signer == address(0)) revert InvalidSignature("No signer installed");

        (address recovered, ECDSA.RecoverError err,) = ECDSA.tryRecover(hash, signature);
        if (err != ECDSA.RecoverError.NoError) revert InvalidSignature("ECDSA recovery failed");
        if (recovered != signer) revert InvalidSignature("Signature signer does not match installed signer");

        return IERC1271.isValidSignature.selector;
    }
}
