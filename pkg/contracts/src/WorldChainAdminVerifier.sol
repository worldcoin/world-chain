// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IERC1271} from "@openzeppelin/contracts/interfaces/IERC1271.sol";

import {IWorldChainAccountHooks} from "./interfaces/IWorldChainAccountHooks.sol";

/// @title WorldChainAdminVerifier
/// @author 0xOsiris, World Contributors
/// @notice An admin verifier implementation for a World Chain account. Each account has one
///         immutable EIP-1271 compliant admin signer; admin verifier implementations MUST
///         implement EIP-1271 and `IWorldChainAccountHooks`.
/// @custom:security-contact security@toolsforhumanity.com
contract WorldChainAdminVerifier is IERC1271, IWorldChainAccountHooks {
    /// @inheritdoc IWorldChainAccountHooks
    function install(bytes calldata installation) external {
        // TODO: FIXME
    }

    /// @inheritdoc IERC1271
    function isValidSignature(bytes32 hash, bytes memory signature) external view returns (bytes4 magicValue) {
        // TODO: FIXME
    }
}
