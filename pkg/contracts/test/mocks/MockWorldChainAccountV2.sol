// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {WorldChainAccount} from "../../src/WorldChainAccount.sol";

/// @notice Drop-in replacement for `WorldChainAccount` used to exercise the beacon upgrade flow.
///         The contract keeps the inherited storage layout intact (so existing accounts retain
///         their admin verifier and key ring across upgrades) and adds a new external function
///         that returns a sentinel value. Tests call that new function on every proxy to confirm
///         all `BeaconProxy` instances pick up the new implementation atomically after the
///         beacon's `upgradeTo` call.
/// @dev `WorldChainAccount.VERSION` is a `public constant` and therefore cannot be overridden by
///      a child contract. To avoid that constraint we expose a separate `v2Version()` getter
///      whose presence is itself proof of the upgrade.
contract MockWorldChainAccountV2 is WorldChainAccount {
    bytes32 public constant V2_SENTINEL = keccak256("MockWorldChainAccountV2.sentinel");

    constructor(address manager) WorldChainAccount(manager) {}

    /// @notice Selector-disjoint from V1 — calling this through a proxy reverts with
    ///         `UnknownSelector` while the V1 implementation is routed and returns
    ///         `V2_SENTINEL` once V2 is the implementation.
    function v2Sentinel() external pure returns (bytes32) {
        return V2_SENTINEL;
    }

    /// @notice Companion getter used by tests to assert the proxy is now resolving V2. We don't
    ///         override `VERSION` because in `WorldChainAccount` it is a `public constant`.
    function v2Version() external pure returns (uint8) {
        return 2;
    }
}
