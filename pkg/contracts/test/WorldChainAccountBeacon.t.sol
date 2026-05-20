// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {UpgradeableBeacon} from "@openzeppelin/contracts/proxy/beacon/UpgradeableBeacon.sol";

import {IWorldChainAccount} from "../src/interfaces/IWorldChainAccount.sol";
import {IWorldChainAccountRouterErrors} from "../src/interfaces/IWorldChainAccountRouterErrors.sol";

import {WorldChainAccountTestSetup} from "./WorldChainAccountTestSetup.sol";
import {MockWorldChainAccountV2} from "./mocks/MockWorldChainAccountV2.sol";

/// @dev ERC-1967 beacon storage slot — `bytes32(uint256(keccak256("eip1967.proxy.beacon")) - 1)`.
bytes32 constant ERC1967_BEACON_SLOT = 0xa3f0ad74e5423aebfd80d3ef4346578335a9a72aeaee59ff6cb3582b35133d50;

/// @title WorldChainAccountBeacon upgrade & proxy tests
/// @notice Locks in the WIP-1001 beacon-proxy contract:
///
///         - The single global beacon is owned by `BEACON_OWNER` and only that owner can rotate
///           the implementation.
///         - Every `WorldChainAccountBeaconProxy` resolves to the predeploy beacon address and
///           the proxy stores the beacon in the canonical ERC-1967 beacon slot.
///         - A single `upgradeTo` on the beacon atomically retargets every live proxy without
///           per-proxy migration calls.
///         - Storage written through a proxy is preserved across implementation upgrades.
///         - The implementation contract has no callable storage of its own (delegatecall-only).
contract WorldChainAccountBeaconTest is WorldChainAccountTestSetup {
    /// @dev Mirror of OpenZeppelin's `UpgradeableBeacon.Upgraded`. Defined here so the test can
    ///      `vm.expectEmit` against it without importing the event from OZ.
    event Upgraded(address indexed implementation);

    // ─── Construction & ownership ─────────────────────────────────────────────

    function test_beacon_initialState() public view {
        assertEq(beacon.implementation(), address(accountImpl), "initial impl");
        assertEq(beacon.owner(), BEACON_OWNER, "owner is the beacon owner");
    }

    function test_beacon_proxyTargetsPredeployBeacon() public view {
        // The proxy MUST store the beacon address in the ERC-1967 beacon slot. This is the
        // contract under test for WIP-1001 — every account address resolves the same beacon.
        bytes32 raw = vm.load(address(account), ERC1967_BEACON_SLOT);
        address resolvedBeacon = address(uint160(uint256(raw)));
        assertEq(resolvedBeacon, BEACON_PREDEPLOY, "BeaconProxy MUST resolve the predeploy beacon");
    }

    function test_beacon_proxyDelegatesToImplementation() public view {
        // The proxy reports the implementation's MANAGER and VERSION because it delegatecalls into
        // it. The implementation itself was constructed with the same MANAGER.
        assertEq(account.MANAGER(), accountImpl.MANAGER());
        assertEq(account.VERSION(), accountImpl.VERSION());
    }

    function test_beacon_implementationHasIndependentStorage() public {
        // The implementation contract has its own storage namespace — calls to it directly do not
        // share storage with any proxy.
        installAdminAs(account, address(happyVerifier));

        // Reading the impl's `ADMIN_VERIFIER` does not see what was written through the proxy.
        assertEq(IWorldChainAccount(address(accountImpl)).ADMIN_VERIFIER(), address(0));
    }

    // ─── upgradeTo access control ─────────────────────────────────────────────

    function test_upgradeTo_revertsForNonOwner() public {
        MockWorldChainAccountV2 v2 = new MockWorldChainAccountV2(MANAGER);

        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, ATTACKER));
        vm.prank(ATTACKER);
        beacon.upgradeTo(address(v2));

        // Implementation pointer unchanged.
        assertEq(beacon.implementation(), address(accountImpl));
    }

    function test_upgradeTo_revertsForNonContractImplementation() public {
        vm.expectRevert(abi.encodeWithSelector(UpgradeableBeacon.BeaconInvalidImplementation.selector, address(0xC0DE)));
        vm.prank(BEACON_OWNER);
        beacon.upgradeTo(address(0xC0DE));
    }

    function test_upgradeTo_revertsForZeroImplementation() public {
        vm.expectRevert(abi.encodeWithSelector(UpgradeableBeacon.BeaconInvalidImplementation.selector, address(0)));
        vm.prank(BEACON_OWNER);
        beacon.upgradeTo(address(0));
    }

    function test_upgradeTo_emitsUpgradedEvent() public {
        MockWorldChainAccountV2 v2 = new MockWorldChainAccountV2(MANAGER);
        vm.expectEmit(true, true, true, true, address(beacon));
        emit Upgraded(address(v2));
        vm.prank(BEACON_OWNER);
        beacon.upgradeTo(address(v2));
        assertEq(beacon.implementation(), address(v2));
    }

    // ─── Atomic upgrade propagation ───────────────────────────────────────────

    function test_upgradeTo_atomicallyUpgradesAllProxies() public {
        IWorldChainAccount a1 = account;
        IWorldChainAccount a2 = newAccountProxy();
        IWorldChainAccount a3 = newAccountProxy();

        // Pre-upgrade everyone is V1 — `VERSION` is the inherited V1 constant, and the V2-only
        // selector reverts via V1's `Dispatch` fallback. We use a low-level call so the test
        // depends only on the revert behaviour (it does not pin the encoded `UnknownSelector`
        // payload; that exact byte-level assertion lives in the dispatch unit tests).
        assertEq(a1.VERSION(), 1);
        assertEq(a2.VERSION(), 1);
        assertEq(a3.VERSION(), 1);
        (bool ok,) = address(a1).call(abi.encodeWithSelector(MockWorldChainAccountV2.v2Sentinel.selector));
        assertFalse(ok, "V2-only selector MUST revert through the V1 dispatch fallback");

        MockWorldChainAccountV2 v2 = new MockWorldChainAccountV2(MANAGER);
        vm.prank(BEACON_OWNER);
        beacon.upgradeTo(address(v2));

        // Post-upgrade the V2-only selector is reachable on every proxy — no per-proxy migration
        // call was needed.
        bytes32 sentinel = MockWorldChainAccountV2(payable(address(a1))).v2Sentinel();
        assertEq(sentinel, v2.V2_SENTINEL());
        assertEq(MockWorldChainAccountV2(payable(address(a2))).v2Sentinel(), sentinel);
        assertEq(MockWorldChainAccountV2(payable(address(a3))).v2Sentinel(), sentinel);
        assertEq(MockWorldChainAccountV2(payable(address(a1))).v2Version(), 2);
    }

    function test_upgradeTo_proxiesDeployedAfterUpgradeUseNewImpl() public {
        MockWorldChainAccountV2 v2 = new MockWorldChainAccountV2(MANAGER);
        vm.prank(BEACON_OWNER);
        beacon.upgradeTo(address(v2));

        IWorldChainAccount fresh = newAccountProxy();
        assertEq(MockWorldChainAccountV2(payable(address(fresh))).v2Sentinel(), v2.V2_SENTINEL());
        assertEq(MockWorldChainAccountV2(payable(address(fresh))).v2Version(), 2);
    }

    // ─── Storage preservation across upgrade ──────────────────────────────────

    function test_upgrade_preservesPerAccountStorage() public {
        installAdminAs(account, address(happyVerifier));
        installSingleSessionKeyRing(account, address(secondHappyVerifier));
        bytes32 keyringHashBefore = account.KEYRING_HASH();
        address adminBefore = account.ADMIN_VERIFIER();

        // Roll the beacon to V2.
        MockWorldChainAccountV2 v2 = new MockWorldChainAccountV2(MANAGER);
        vm.prank(BEACON_OWNER);
        beacon.upgradeTo(address(v2));

        // V2 is now the routed implementation (selector check) and EIP-7201 keyring storage is
        // preserved bit-for-bit across the implementation swap.
        assertEq(MockWorldChainAccountV2(payable(address(account))).v2Version(), 2);
        assertEq(account.ADMIN_VERIFIER(), adminBefore);
        assertEq(account.KEYRING_HASH(), keyringHashBefore);
        assertEq(account.sessionKeyRing(address(secondHappyVerifier)), keyringHashBefore);
    }

    function test_upgrade_doesNotResetAdminInstallationGuard() public {
        installAdminAs(account, address(happyVerifier));

        MockWorldChainAccountV2 v2 = new MockWorldChainAccountV2(MANAGER);
        vm.prank(BEACON_OWNER);
        beacon.upgradeTo(address(v2));

        // The "admin already installed" guard MUST still fire after upgrade — the upgrade is a
        // pure code-pointer swap, no storage reset.
        vm.expectRevert(IWorldChainAccountRouterErrors.AdminAlreadyInstalled.selector);
        vm.prank(MANAGER);
        account.installAdmin(descriptor(address(secondHappyVerifier)));
    }

    function test_upgrade_canBeRolledBack() public {
        installAdminAs(account, address(happyVerifier));

        MockWorldChainAccountV2 v2 = new MockWorldChainAccountV2(MANAGER);
        vm.prank(BEACON_OWNER);
        beacon.upgradeTo(address(v2));
        assertEq(MockWorldChainAccountV2(payable(address(account))).v2Version(), 2);

        // Roll back to V1 — beacon must accept it just like any other valid implementation.
        vm.prank(BEACON_OWNER);
        beacon.upgradeTo(address(accountImpl));
        assertEq(account.VERSION(), 1);
        assertEq(account.ADMIN_VERIFIER(), address(happyVerifier));

        // V2-only selector now reverts again because dispatch is back on V1.
        (bool ok,) = address(account).call(abi.encodeWithSelector(MockWorldChainAccountV2.v2Sentinel.selector));
        assertFalse(ok, "V2 selector MUST revert once rolled back to V1");
    }

    // ─── Beacon attack surface ────────────────────────────────────────────────

    function test_attack_proxyCannotMutateBeaconSlot() public {
        // Even though the BeaconProxy reads beacon from the ERC-1967 slot, it MUST never let
        // the implementation overwrite that slot. We assert the slot is unchanged after the
        // implementation has executed arbitrary state mutations (admin + keyring installs).
        bytes32 before = vm.load(address(account), ERC1967_BEACON_SLOT);
        installAdminAs(account, address(happyVerifier));
        installSingleSessionKeyRing(account, address(secondHappyVerifier));
        bytes32 afterMutations = vm.load(address(account), ERC1967_BEACON_SLOT);
        assertEq(before, afterMutations, "BEACON_SLOT MUST be immutable from impl context");
    }

    function test_attack_implementationCannotBeCalledAsAccount() public {
        // Calling the implementation directly bypasses no checks — `onlyManager` still rejects
        // arbitrary callers. The point is that even if an attacker could reach the impl directly,
        // the manager check still holds.
        vm.expectRevert(IWorldChainAccountRouterErrors.CallerNotManager.selector);
        vm.prank(ATTACKER);
        IWorldChainAccount(address(accountImpl)).installAdmin(descriptor(address(happyVerifier)));
    }

    function test_attack_ownerOnlyTransfersViaOwnable() public {
        // The beacon is `Ownable`; ownership transfer is gated by the same access-control as
        // `upgradeTo`. Confirm attacker cannot grab ownership.
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, ATTACKER));
        vm.prank(ATTACKER);
        beacon.transferOwnership(ATTACKER);

        // Owner can transfer.
        address newOwner = address(0xBEEF);
        vm.prank(BEACON_OWNER);
        beacon.transferOwnership(newOwner);
        assertEq(beacon.owner(), newOwner);

        // New owner can upgrade, old owner cannot.
        MockWorldChainAccountV2 v2 = new MockWorldChainAccountV2(MANAGER);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, BEACON_OWNER));
        vm.prank(BEACON_OWNER);
        beacon.upgradeTo(address(v2));

        vm.prank(newOwner);
        beacon.upgradeTo(address(v2));
        assertEq(beacon.implementation(), address(v2));
    }

    function test_attack_renounceOwnershipFreezesBeacon() public {
        // After renouncing ownership, the beacon's implementation is effectively frozen.
        vm.prank(BEACON_OWNER);
        beacon.renounceOwnership();
        assertEq(beacon.owner(), address(0));

        MockWorldChainAccountV2 v2 = new MockWorldChainAccountV2(MANAGER);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, BEACON_OWNER));
        vm.prank(BEACON_OWNER);
        beacon.upgradeTo(address(v2));
    }
}
