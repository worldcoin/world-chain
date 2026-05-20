// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IERC1271} from "@openzeppelin/contracts/interfaces/IERC1271.sol";

import {IWorldChainAccount} from "../../src/interfaces/IWorldChainAccount.sol";
import {IWorldChainSessionVerifier} from "../../src/interfaces/IWorldChainSessionVerifier.sol";
import {WorldChainAccountVerifier} from "../../src/interfaces/IWorldChainAccountManager.sol";

/// @notice Storage slot used by verifier mocks to write installation evidence into the
///         delegate-caller's (i.e. the World Chain account's) storage. The tests read this slot
///         directly from the account address to assert the install hook ran via `DELEGATECALL`.
library MockVerifierStorage {
    bytes32 internal constant INSTALL_EVIDENCE_SLOT = keccak256("mock.verifier.install.evidence");
}

/// @notice Minimal verifier mock that accepts every `EIP-1271` signature and allows every policy
///         evaluation. The default happy-path verifier used in unit tests. Stamps a deterministic
///         witness derived from `installation` into the account's storage from inside the install
///         hook so tests can confirm the hook ran via `DELEGATECALL`.
contract MockHappyVerifier is IWorldChainSessionVerifier {
    function install(bytes calldata installation) external override {
        bytes32 slot = MockVerifierStorage.INSTALL_EVIDENCE_SLOT;
        bytes32 value = keccak256(installation);
        assembly ("memory-safe") {
            sstore(slot, value)
        }
    }

    function isValidSignature(bytes32, bytes memory) public pure override returns (bytes4) {
        return IERC1271.isValidSignature.selector;
    }

    function evaluateSessionPolicy(ExecutionTraceContext calldata) external pure override returns (bool) {
        return true;
    }
}

/// @notice Mirror of `MockHappyVerifier` that rejects every signature and denies every policy
///         evaluation. Used to exercise the negative branch of dispatch helpers.
contract MockRejectingVerifier is IWorldChainSessionVerifier {
    function install(bytes calldata) external override {}

    function isValidSignature(bytes32, bytes memory) public pure override returns (bytes4) {
        return bytes4(0xffffffff);
    }

    function evaluateSessionPolicy(ExecutionTraceContext calldata) external pure override returns (bool) {
        return false;
    }
}

/// @notice Verifier whose `install` hook reverts with a custom error. Verifies that
///         `Address.functionDelegateCall` bubbles the callee's revert through the account.
contract MockRevertingInstallVerifier is IWorldChainSessionVerifier {
    error InstallReverted(bytes payload);

    function install(bytes calldata installation) external pure override {
        revert InstallReverted(installation);
    }

    function isValidSignature(bytes32, bytes memory) public pure override returns (bytes4) {
        return IERC1271.isValidSignature.selector;
    }

    function evaluateSessionPolicy(ExecutionTraceContext calldata) external pure override returns (bool) {
        return true;
    }
}

/// @notice Verifier whose `isValidSignature` reverts. Used to test bubble-up from the EIP-1271
///         dispatch path.
contract MockRevertingSignatureVerifier is IWorldChainSessionVerifier {
    error SignatureCheckReverted();

    function install(bytes calldata) external override {}

    function isValidSignature(bytes32, bytes memory) public pure override returns (bytes4) {
        revert SignatureCheckReverted();
    }

    function evaluateSessionPolicy(ExecutionTraceContext calldata) external pure override returns (bool) {
        return true;
    }
}

/// @notice Verifier that, during `install`, attempts to re-enter the account router and call
///         `installAdmin` again. Confirms `onlyManager` cannot be bypassed via the install hook
///         (the nested call's `msg.sender` is the account itself, not the manager).
contract MockReentrantAdminInstaller is IWorldChainSessionVerifier {
    function install(bytes calldata) external override {
        // We are executing in the account's storage via DELEGATECALL.
        // address(this) == account here. The re-entrant call below is a normal CALL
        // from the account to itself, so msg.sender becomes the account, not the
        // manager — `onlyManager` MUST reject.
        WorldChainAccountVerifier memory inner =
            WorldChainAccountVerifier({verifier: address(0xbeef), installation: ""});
        (bool ok, bytes memory ret) = address(this).call(abi.encodeCall(IWorldChainAccount.installAdmin, (inner)));
        assembly ("memory-safe") {
            switch ok
            case 0 { revert(add(ret, 32), mload(ret)) }
            default { return(add(ret, 32), mload(ret)) }
        }
    }

    function isValidSignature(bytes32, bytes memory) public pure override returns (bytes4) {
        return IERC1271.isValidSignature.selector;
    }

    function evaluateSessionPolicy(ExecutionTraceContext calldata) external pure override returns (bool) {
        return true;
    }
}

/// @notice Verifier that writes a sentinel into a deterministic slot during `install`. Used to
///         confirm the install hook executed in the account's storage namespace (i.e. via
///         `DELEGATECALL`) and not in the verifier's own storage.
contract MockStorageWriter is IWorldChainSessionVerifier {
    /// @notice Slot the test reads from the account address to assert delegatecall semantics.
    bytes32 public constant SENTINEL_SLOT = keccak256("mock.verifier.sentinel");
    bytes32 public constant SENTINEL_VALUE = bytes32(uint256(0xc0ffeec0ffeec0ffeec0ffeec0ffeec0));

    function install(bytes calldata) external override {
        bytes32 slot = SENTINEL_SLOT;
        bytes32 value = SENTINEL_VALUE;
        assembly ("memory-safe") {
            sstore(slot, value)
        }
    }

    function isValidSignature(bytes32, bytes memory) public pure override returns (bytes4) {
        return IERC1271.isValidSignature.selector;
    }

    function evaluateSessionPolicy(ExecutionTraceContext calldata) external pure override returns (bool) {
        return true;
    }
}
