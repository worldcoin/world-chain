// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IERC1271} from "@openzeppelin/contracts/interfaces/IERC1271.sol";

import {IWorldChainAccountHooks} from "./interfaces/IWorldChainAccountHooks.sol";
import {WorldChainAccountVerifier} from "./interfaces/IWorldChainAccountManager.sol";
import {IWorldChainAccountRouter} from "./interfaces/IWorldChainAccountRouter.sol";
import {IWorldChainAccountRouterErrors} from "./interfaces/IWorldChainAccountRouterErrors.sol";
import {IWorldChainSessionVerifier} from "./interfaces/IWorldChainSessionVerifier.sol";

/// @title WorldChainAccountRouter
/// @author 0xOsiris, World Contributors
contract WorldChainAccountRouter is IWorldChainAccountRouter, IWorldChainAccountRouterErrors {
    // ─── Selectors ───────────────────────────────────────────────────────

    /// @dev Selector for `IERC1271.isValidSignature(bytes32,bytes)`.
    bytes4 private constant SIG_SELECTOR = IERC1271.isValidSignature.selector;

    // ─── Immutables ──────────────────────────────────────────────────────

    /// @notice `WORLD_CHAIN_ACCOUNT_MANAGER` predeploy address. The sole authorized caller of
    ///         `installAdmin` and `installKeyRing`.
    address public immutable MANAGER;

    // ─── Diamond storage ─────────────────────────────────────────────────

    /// @notice Router-owned storage. Held under a namespaced slot so verifier installation
    ///         namespaces cannot collide with router state.
    /// @param adminVerifier The immutable admin verifier implementation address. Set by the
    ///                      single permitted `installAdmin` invocation and never overwritten.
    /// @param keyRingHash The active key ring's canonical hash,
    ///                    `keccak256(abi.encode(sessionVerifiers))`. Matches the WIP-1001
    ///                    manager-side `keyRingHash` and serves as the per-account generation
    ///                    marker for session-verifier membership.
    /// @param sessionKeyRing Per-verifier installation marker — the `keyRingHash` at the time
    ///                      the verifier was installed. A verifier `v` is a member of the
    ///                      active key ring iff `sessionKeyRing[v] == keyRingHash != 0`. Because
    ///                      the hash commits to the full ordered set of `(verifier, installation)`
    ///                      tuples, a stale marker can only collide with the current hash when
    ///                      the active ring is byte-identical — in which case `v` is genuinely
    ///                      a member, so the equality test is exact.
    struct RouterStorage {
        address adminVerifier;
        bytes32 keyRingHash;
        mapping(address verifier => bytes32 keyRingHash) sessionKeyRing;
    }

    /// @dev EIP-7201-style namespaced storage slot for `RouterStorage`.
    bytes32 private constant ROUTER_STORAGE_SLOT = keccak256("account.router.storage.v1");

    // ─── Construction ────────────────────────────────────────────────────

    /// @param manager The `WORLD_CHAIN_ACCOUNT_MANAGER` predeploy address.
    constructor(address manager) {
        MANAGER = manager;
    }

    // ─── Modifiers ───────────────────────────────────────────────────────

    /// @notice Restricts the call to `WORLD_CHAIN_ACCOUNT_MANAGER`.
    modifier onlyManager() {
        if (msg.sender != MANAGER) revert CallerNotManager();
        _;
    }

    /// @notice Requires `verifier` to be a member of the active key ring. Reverts before any
    ///         verifier code can execute.
    modifier sessionInstalled(address verifier) {
        RouterStorage storage $ = __storage();
        bytes32 hash = $.keyRingHash;
        if (hash == bytes32(0) || $.sessionKeyRing[verifier] != hash) {
            revert VerifierNotInstalled(verifier);
        }
        _;
    }

    /// @inheritdoc IWorldChainAccountRouter
    function installAdmin(WorldChainAccountVerifier calldata admin) external onlyManager {
        if (admin.verifier == address(0)) revert ZeroAdminVerifier();
        RouterStorage storage $ = __storage();
        if ($.adminVerifier != address(0)) revert AdminAlreadyInstalled();
        $.adminVerifier = admin.verifier;
        _install(admin.verifier, admin.installation);
    }

    /// @inheritdoc IWorldChainAccountRouter
    function installKeyRing(WorldChainAccountVerifier[] calldata sessionVerifiers) external onlyManager {
        RouterStorage storage $ = __storage();
        // The WIP-1001 canonical key-ring hash. Rotating to a new hash atomically retires every
        // entry from the prior key ring without an explicit per-element wipe.
        bytes32 hash = keccak256(abi.encode(sessionVerifiers));
        $.keyRingHash = hash;
        uint256 n = sessionVerifiers.length;
        for (uint256 i; i < n; ++i) {
            WorldChainAccountVerifier calldata v = sessionVerifiers[i];
            $.sessionKeyRing[v.verifier] = hash;
            _install(v.verifier, v.installation);
        }
    }

    /// @inheritdoc IWorldChainAccountRouter
    function isValidSignatureForAdmin(bytes32, bytes calldata) external returns (bytes4 magicValue) {
        address admin = __storage().adminVerifier;
        if (admin == address(0)) revert AdminNotInstalled();
        // Local args (bytes32, bytes) match IERC1271.isValidSignature(bytes32, bytes) exactly,
        // so swap the leading selector in place rather than re-encode through Solidity.
        _delegate(admin, _spliceCurrentSelector(SIG_SELECTOR));
        assembly ("memory-safe") {
            magicValue := mload(0)
        }
    }

    /// @inheritdoc IWorldChainAccountRouter
    function isValidSignatureForVerifier(address verifier, bytes32 hash, bytes calldata signature)
        external
        sessionInstalled(verifier)
        returns (bytes4 magicValue)
    {
        _delegate(verifier, abi.encodeCall(IERC1271.isValidSignature, (hash, signature)));
        assembly ("memory-safe") {
            magicValue := mload(0)
        }
    }

    /// @inheritdoc IWorldChainAccountRouter
    function evaluateSessionPolicyForVerifier(
        address verifier,
        IWorldChainSessionVerifier.ExecutionTraceContext calldata context
    ) external sessionInstalled(verifier) returns (bool allowed) {
        _delegate(verifier, abi.encodeCall(IWorldChainSessionVerifier.evaluateSessionPolicy, (context)));
        assembly ("memory-safe") {
            allowed := mload(0)
        }
    }

    /// @dev DELEGATECALLs `target` with `payload`, unconditionally copying the verifier's
    ///      returndata to memory offset 0. On failure the copied returndata is propagated
    ///      verbatim via `revert`. On success control returns to the Solidity caller; the
    ///      verifier's returndata is left at offset 0 and validation entry points consume it
    ///      directly via `mload(0)` as their result (the `EIP1271_MAGIC_VALUE` `bytes4` or the
    ///      session-policy `bool`).
    function _delegate(address target, bytes memory payload) private {
        assembly ("memory-safe") {
            let ok := delegatecall(gas(), target, add(payload, 0x20), mload(payload), 0, 0)
            let size := returndatasize()
            returndatacopy(0, 0, size)
            if iszero(ok) { revert(0, size) }
        }
    }

    /// @dev Encodes `IWorldChainAccountHooks.install(installation)` and DELEGATECALLs `verifier`
    ///      via the shared `_delegate` primitive.
    function _install(address verifier, bytes calldata installation) private {
        _delegate(verifier, abi.encodeCall(IWorldChainAccountHooks.install, (installation)));
    }

    /// @dev Builds a new dispatch payload by replacing the leading 4-byte selector of the
    ///      current calldata with `selector`. Returns a `bytes memory` whose data is laid out
    ///      as `[selector || calldata[4:]]`, suitable for `_delegate`/`_forward`. Used when the
    ///      dispatching function's argument ABI matches the verifier function's ABI exactly,
    ///      avoiding the re-encode cost of `abi.encodeCall`.
    function _spliceCurrentSelector(bytes4 selector) private pure returns (bytes memory payload) {
        assembly ("memory-safe") {
            payload := mload(0x40)
            let cdSize := calldatasize()
            // Solidity `bytes` memory layout: 32-byte length, then data.
            mstore(payload, cdSize)
            // Write the new selector, left-aligned in the first data word.
            mstore(add(payload, 0x20), selector)
            // Copy the original calldata args (tail starting at offset 4) over the right
            // padding of the selector word, completing the payload.
            calldatacopy(add(payload, 0x24), 4, sub(cdSize, 4))
            // Advance and 32-align the free-memory pointer past payload[0 .. 32 + cdSize].
            let endPtr := add(add(payload, 0x20), cdSize)
            mstore(0x40, and(add(endPtr, 0x1f), not(0x1f)))
        }
    }

    function __storage() private pure returns (RouterStorage storage $) {
        bytes32 slot = ROUTER_STORAGE_SLOT;
        assembly ("memory-safe") {
            $.slot := slot
        }
    }
}
