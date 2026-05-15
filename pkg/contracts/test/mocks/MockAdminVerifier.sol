// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IERC1271} from "@openzeppelin/contracts/interfaces/IERC1271.sol";
import {IWorldChainAccountHooks} from "../../src/interfaces/IWorldChainAccountHooks.sol";

/// @title MockAdminVerifier
/// @notice Minimal admin verifier used by the WIP-1001 tests. The signature is a single byte:
///         `0x01` accepts, anything else rejects. `install` is a no-op so tests do not depend
///         on installation side effects.
/// @dev The verifier captures its own deployed address into `_SELF` so that, if any future
///      configuration is added, it can be read via an external `STATICCALL` to `_SELF` from
///      inside a `DELEGATECALL` frame.
contract MockAdminVerifier is IERC1271, IWorldChainAccountHooks {
    bytes4 internal constant MAGIC = 0x1626ba7e;
    bytes4 internal constant FAILURE = 0xffffffff;

    address internal immutable _SELF;

    /// @notice Counts the number of `install` calls observed on the router's namespaced slot.
    /// @dev Stored at slot `keccak256(abi.encode("MockAdminVerifier.installs", _SELF))` so
    ///      multiple installations of the same mock at the same account do not collide with
    ///      other verifiers.
    function installs() external view returns (uint256 count) {
        bytes32 slot = keccak256(abi.encode("MockAdminVerifier.installs", _SELF));
        assembly {
            count := sload(slot)
        }
    }

    constructor() {
        _SELF = address(this);
    }

    function install(bytes calldata) external override {
        bytes32 slot = keccak256(abi.encode("MockAdminVerifier.installs", _SELF));
        assembly {
            sstore(slot, add(sload(slot), 1))
        }
    }

    function isValidSignature(bytes32, bytes memory signature) external pure override returns (bytes4) {
        if (signature.length == 1 && signature[0] == 0x01) return MAGIC;
        return FAILURE;
    }
}
