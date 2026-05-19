// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IERC1271} from "@openzeppelin/contracts/interfaces/IERC1271.sol";

import {IWorldChainAccountHooks} from "./interfaces/IWorldChainAccountHooks.sol";
import {IWorldChainSessionVerifier} from "./interfaces/IWorldChainSessionVerifier.sol";

/// @title WorldChainSessionVerifier
/// @author 0xOsiris, World Contributors
/// @notice A session verifier implementation for a World Chain account. Session verifiers act as
///         the transaction level signatories via programmable EIP-1271 smart contracts. Session
///         verifiers dually function as signature verifiers, and session key policy evaluators.
/// @custom:security-contact security@toolsforhumanity.com
contract WorldChainSessionVerifier is IWorldChainSessionVerifier {
    /// @inheritdoc IWorldChainAccountHooks
    function install(bytes calldata installation) external {
        // TODO: FIXME
    }

    /// @inheritdoc IERC1271
    function isValidSignature(bytes32 hash, bytes memory signature) external view returns (bytes4 magicValue) {
        // TODO: FIXME
    }

    /// @inheritdoc IWorldChainSessionVerifier
    function evaluateSessionPolicy(ExecutionTraceContext calldata context) external view returns (bool allowed) {
        // TODO: FIXME
    }
}
