// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IWorldChainAccountHooks} from "../../src/interfaces/IWorldChainAccountHooks.sol";
import {IWorldChainSessionVerifier} from "../../src/interfaces/IWorldChainSessionVerifier.sol";

/// @title MockSessionVerifier
/// @notice Minimal session verifier used by the WIP-1001 tests. The signature is a single byte:
///         `0x01` accepts, anything else rejects. Policy is always-allow. `install` is a no-op
///         apart from incrementing an install counter at a namespaced slot.
contract MockSessionVerifier is IWorldChainSessionVerifier {
    bytes4 internal constant MAGIC = 0x1626ba7e;
    bytes4 internal constant FAILURE = 0xffffffff;

    address internal immutable _SELF;

    constructor() {
        _SELF = address(this);
    }

    function installs() external view returns (uint256 count) {
        bytes32 slot = keccak256(abi.encode("MockSessionVerifier.installs", _SELF));
        assembly {
            count := sload(slot)
        }
    }

    function install(bytes calldata) external override {
        bytes32 slot = keccak256(abi.encode("MockSessionVerifier.installs", _SELF));
        assembly {
            sstore(slot, add(sload(slot), 1))
        }
    }

    function isValidSignature(bytes32, bytes memory signature) external pure override returns (bytes4) {
        if (signature.length == 1 && signature[0] == 0x01) return MAGIC;
        return FAILURE;
    }

    function evaluateSessionPolicy(ExecutionTraceContext calldata) external pure override returns (bool) {
        return true;
    }
}
