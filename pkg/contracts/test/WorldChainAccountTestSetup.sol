// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";

import {WorldChainAccount} from "../src/WorldChainAccount.sol";
import {WorldChainAccountBeaconProxy} from "../src/WorldChainAccountBeaconProxy.sol";
import {WorldChainAccountUpgradeableBeacon} from "../src/WorldChainAccountUpgradeableBeacon.sol";
import {IWorldChainAccount} from "../src/interfaces/IWorldChainAccount.sol";
import {WorldChainAccountVerifier} from "../src/interfaces/IWorldChainAccountManager.sol";

import {
    MockHappyVerifier,
    MockRejectingVerifier,
    MockRevertingInstallVerifier,
    MockRevertingSignatureVerifier,
    MockReentrantAdminInstaller,
    MockStorageWriter
} from "./mocks/MockWorldChainVerifier.sol";

/// @title WorldChainAccountTestSetup
/// @notice Reusable, monomorphic harness for the WIP-1001 `WorldChainAccount` router. Every
///         derived test inherits from this contract and gets a fully-wired environment:
///
///         - The canonical `WorldChainAccount` implementation deployed once and reused.
///         - The `WorldChainAccountUpgradeableBeacon` etched at the WIP-1001 predeploy address
///           (`0x000000000000000000000000000000000000B0cC`), pointed at the implementation, with
///           a known beacon owner so upgrade-path tests can re-target it.
///         - A `WorldChainAccountBeaconProxy` instance acting as the account address tests
///           operate against.
///         - A set of mock verifier implementations covering the full happy-path and adversarial
///           matrix (rejecting, reverting, re-entrant, and storage-clobbering).
contract WorldChainAccountTestSetup is Test {
    // ─── Constants ────────────────────────────────────────────────────────────

    /// @notice Hardcoded beacon predeploy address consumed by `WorldChainAccountBeaconProxy`.
    address internal constant BEACON_PREDEPLOY = 0x000000000000000000000000000000000000B0cC;

    /// @notice Address used as the `MANAGER` baked into the V1 implementation. Tests `vm.prank`
    ///         this address when exercising the manager-only entry points.
    address internal constant MANAGER = address(0x000000000000000000000000000000000000B0CD);

    /// @notice Address used as the beacon owner. Distinct from `MANAGER` so tests can confirm the
    ///         beacon's `onlyOwner` access control is enforced independently of the account-level
    ///         manager check.
    address internal constant BEACON_OWNER = address(0xBEAC0); // 0xBEAC0

    /// @notice Address used as a plain end-user EOA in negative tests.
    address internal constant ATTACKER = address(0xA11CE);

    // ─── Wiring ───────────────────────────────────────────────────────────────

    /// @notice The currently-routed `WorldChainAccount` implementation.
    WorldChainAccount internal accountImpl;

    /// @notice The single beacon predeploy. After `setUp()` runs the contract at this address is
    ///         the freshly-deployed `WorldChainAccountUpgradeableBeacon`.
    WorldChainAccountUpgradeableBeacon internal beacon;

    /// @notice The default account proxy under test. Derived tests can deploy additional proxies
    ///         to exercise multi-account scenarios (e.g. atomic upgrade propagation).
    IWorldChainAccount internal account;

    // ─── Mock verifiers ───────────────────────────────────────────────────────

    MockHappyVerifier internal happyVerifier;
    MockHappyVerifier internal secondHappyVerifier;
    MockRejectingVerifier internal rejectingVerifier;
    MockRevertingInstallVerifier internal revertingInstallVerifier;
    MockRevertingSignatureVerifier internal revertingSignatureVerifier;
    MockReentrantAdminInstaller internal reentrantAdminInstaller;
    MockStorageWriter internal storageWriter;

    // ─── Setup ────────────────────────────────────────────────────────────────

    function setUp() public virtual {
        accountImpl = new WorldChainAccount(MANAGER);

        // Install the beacon at the WIP-1001 predeploy address pointed at the V1 implementation
        // and owned by `BEACON_OWNER` so upgrade tests can be authored against a known owner.
        deployCodeTo(
            "WorldChainAccountUpgradeableBeacon.sol:WorldChainAccountUpgradeableBeacon",
            abi.encode(address(accountImpl), BEACON_OWNER),
            BEACON_PREDEPLOY
        );
        beacon = WorldChainAccountUpgradeableBeacon(BEACON_PREDEPLOY);

        account = IWorldChainAccount(address(newAccountProxy()));

        happyVerifier = new MockHappyVerifier();
        secondHappyVerifier = new MockHappyVerifier();
        rejectingVerifier = new MockRejectingVerifier();
        revertingInstallVerifier = new MockRevertingInstallVerifier();
        revertingSignatureVerifier = new MockRevertingSignatureVerifier();
        reentrantAdminInstaller = new MockReentrantAdminInstaller();
        storageWriter = new MockStorageWriter();

        vm.label(address(accountImpl), "WorldChainAccount.impl");
        vm.label(address(beacon), "WorldChainAccountBeacon");
        vm.label(address(account), "WorldChainAccountProxy");
        vm.label(MANAGER, "MANAGER");
        vm.label(BEACON_OWNER, "BeaconOwner");
        vm.label(ATTACKER, "Attacker");
        vm.label(address(happyVerifier), "HappyVerifier");
        vm.label(address(secondHappyVerifier), "HappyVerifier2");
        vm.label(address(rejectingVerifier), "RejectingVerifier");
        vm.label(address(revertingInstallVerifier), "RevertingInstallVerifier");
        vm.label(address(revertingSignatureVerifier), "RevertingSignatureVerifier");
        vm.label(address(reentrantAdminInstaller), "ReentrantAdminInstaller");
        vm.label(address(storageWriter), "StorageWriter");
    }

    // ─── Helpers ──────────────────────────────────────────────────────────────

    /// @notice Deploys a fresh `WorldChainAccountBeaconProxy` resolving the beacon predeploy.
    function newAccountProxy() internal returns (IWorldChainAccount) {
        return IWorldChainAccount(address(new WorldChainAccountBeaconProxy()));
    }

    /// @notice Convenience wrapper that wraps a verifier address in the verifier descriptor with
    ///         empty installation data.
    function descriptor(address verifier) internal pure returns (WorldChainAccountVerifier memory) {
        return WorldChainAccountVerifier({verifier: verifier, installation: ""});
    }

    /// @notice Same as `descriptor` but with the supplied installation payload, used by
    ///         delegatecall-witness tests (e.g. asserting the hook ran in the account's storage).
    function descriptorWithData(address verifier, bytes memory installation)
        internal
        pure
        returns (WorldChainAccountVerifier memory)
    {
        return WorldChainAccountVerifier({verifier: verifier, installation: installation});
    }

    /// @notice Installs `admin` against the account via the manager prank used everywhere in the
    ///         test suite.
    function installAdminAs(IWorldChainAccount account_, address admin) internal {
        vm.prank(MANAGER);
        account_.installAdmin(descriptor(admin));
    }

    /// @notice Installs a one-element key ring containing `verifier` against the account.
    function installSingleSessionKeyRing(IWorldChainAccount account_, address verifier) internal {
        WorldChainAccountVerifier[] memory ring = new WorldChainAccountVerifier[](1);
        ring[0] = descriptor(verifier);
        vm.prank(MANAGER);
        account_.installKeyRing(ring);
    }

    /// @notice Computes the canonical `keyRingHash` for a verifier set the same way the contract
    ///         does. Used to assert the recorded hash after `installKeyRing`.
    function keyRingHashOf(WorldChainAccountVerifier[] memory verifiers) internal pure returns (bytes32) {
        return keccak256(abi.encode(verifiers));
    }
}
