// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IERC1271} from "@openzeppelin/contracts/interfaces/IERC1271.sol";
import {Address} from "@openzeppelin/contracts/utils/Address.sol";

import {WorldChainAccount} from "../src/WorldChainAccount.sol";
import {KeyringStore} from "../src/abstract/KeyringStore.sol";
import {IWorldChainAccount} from "../src/interfaces/IWorldChainAccount.sol";
import {IWorldChainAccountRouterErrors} from "../src/interfaces/IWorldChainAccountRouterErrors.sol";
import {IWorldChainSessionVerifier} from "../src/interfaces/IWorldChainSessionVerifier.sol";
import {WorldChainAccountVerifier} from "../src/interfaces/IWorldChainAccountManager.sol";

import {WorldChainAccountTestSetup} from "./WorldChainAccountTestSetup.sol";
import {
    MockHappyVerifier,
    MockRevertingInstallVerifier,
    MockRevertingSignatureVerifier,
    MockReentrantAdminInstaller,
    MockStorageWriter,
    MockVerifierStorage
} from "./mocks/MockWorldChainVerifier.sol";

/// @title WorldChainAccount unit tests
/// @notice Exercises every external entry point on the WIP-1001 account router (`installAdmin`,
///         `installKeyRing`, the three dispatch helpers, and the dispatch fallback) along with
///         their error paths, attack surface, and the EIP-7201 keyring storage invariants.
contract WorldChainAccountTest is WorldChainAccountTestSetup {
    /// @dev `MAX_KEYRING_SIZE` mirror so the bounds-check test stays in sync if the constant moves.
    uint8 internal constant MAX_KEYRING_SIZE = 20;

    /// @dev Magic value returned by every passing `MockHappyVerifier` and friends.
    bytes4 internal constant ERC1271_MAGIC = IERC1271.isValidSignature.selector;

    // ─── Constructor ──────────────────────────────────────────────────────────

    function test_constructor_setsManager() public view {
        assertEq(account.MANAGER(), MANAGER, "MANAGER should be the address baked into the impl");
        assertEq(accountImpl.MANAGER(), MANAGER, "Impl exposes the same MANAGER");
        assertEq(account.VERSION(), 1, "V1 implementation reports version 1");
    }

    function test_constructor_revertsOnZeroManager() public {
        vm.expectRevert(IWorldChainAccountRouterErrors.AddressZero.selector);
        new WorldChainAccount(address(0));
    }

    // ─── installAdmin ─────────────────────────────────────────────────────────

    function test_installAdmin_succeedsAsManager() public {
        bytes memory payload = bytes("admin-install");
        vm.prank(MANAGER);
        account.installAdmin(descriptorWithData(address(happyVerifier), payload));

        assertEq(account.ADMIN_VERIFIER(), address(happyVerifier));

        // The install hook is delegatecalled, so the witness sstore lands in the ACCOUNT's
        // storage namespace — not the verifier's. Read the slot from the account address.
        bytes32 witness = vm.load(address(account), MockVerifierStorage.INSTALL_EVIDENCE_SLOT);
        assertEq(witness, keccak256(payload), "install hook MUST execute via DELEGATECALL");

        // The verifier itself was not written to.
        bytes32 verifierSlot = vm.load(address(happyVerifier), MockVerifierStorage.INSTALL_EVIDENCE_SLOT);
        assertEq(verifierSlot, bytes32(0), "verifier storage MUST be untouched");
    }

    function test_installAdmin_revertsForNonManager() public {
        vm.expectRevert(IWorldChainAccountRouterErrors.CallerNotManager.selector);
        vm.prank(ATTACKER);
        account.installAdmin(descriptor(address(happyVerifier)));
    }

    function test_installAdmin_revertsForZeroVerifier() public {
        vm.expectRevert(IWorldChainAccountRouterErrors.ZeroAdminVerifier.selector);
        vm.prank(MANAGER);
        account.installAdmin(descriptor(address(0)));
    }

    function test_installAdmin_revertsOnSecondCall() public {
        installAdminAs(account, address(happyVerifier));

        vm.expectRevert(IWorldChainAccountRouterErrors.AdminAlreadyInstalled.selector);
        vm.prank(MANAGER);
        account.installAdmin(descriptor(address(secondHappyVerifier)));

        // Admin remains the original verifier.
        assertEq(account.ADMIN_VERIFIER(), address(happyVerifier));
    }

    function test_installAdmin_bubblesHookRevert() public {
        bytes memory payload = bytes("boom");
        vm.expectRevert(abi.encodeWithSelector(MockRevertingInstallVerifier.InstallReverted.selector, payload));
        vm.prank(MANAGER);
        account.installAdmin(descriptorWithData(address(revertingInstallVerifier), payload));

        // The revert MUST unwind the install, so the admin slot stays empty.
        assertEq(account.ADMIN_VERIFIER(), address(0));
    }

    function test_installAdmin_reentrancyFailsOnManagerCheck() public {
        // The reentrant verifier issues a normal CALL back into `installAdmin`, so the nested
        // frame sees `msg.sender == address(account)`, which fails `onlyManager`. The outer call
        // bubbles `CallerNotManager`.
        vm.expectRevert(IWorldChainAccountRouterErrors.CallerNotManager.selector);
        vm.prank(MANAGER);
        account.installAdmin(descriptor(address(reentrantAdminInstaller)));

        // Outer install fully unwound — admin slot remains empty.
        assertEq(account.ADMIN_VERIFIER(), address(0));
    }

    // ─── installKeyRing ───────────────────────────────────────────────────────

    function test_installKeyRing_succeedsAsManager() public {
        WorldChainAccountVerifier[] memory ring = new WorldChainAccountVerifier[](2);
        ring[0] = descriptor(address(happyVerifier));
        ring[1] = descriptor(address(secondHappyVerifier));

        vm.prank(MANAGER);
        account.installKeyRing(ring);

        bytes32 expected = keyRingHashOf(ring);
        assertEq(account.KEYRING_HASH(), expected);
        assertEq(account.sessionKeyRing(address(happyVerifier)), expected);
        assertEq(account.sessionKeyRing(address(secondHappyVerifier)), expected);
        assertEq(account.sessionKeyRing(address(rejectingVerifier)), bytes32(0));
    }

    function test_installKeyRing_revertsForNonManager() public {
        WorldChainAccountVerifier[] memory ring = new WorldChainAccountVerifier[](1);
        ring[0] = descriptor(address(happyVerifier));
        vm.expectRevert(IWorldChainAccountRouterErrors.CallerNotManager.selector);
        vm.prank(ATTACKER);
        account.installKeyRing(ring);
    }

    function test_installKeyRing_acceptsMaxSize() public {
        WorldChainAccountVerifier[] memory ring = new WorldChainAccountVerifier[](MAX_KEYRING_SIZE);
        for (uint256 i; i < MAX_KEYRING_SIZE; ++i) {
            MockHappyVerifier v = new MockHappyVerifier();
            ring[i] = descriptor(address(v));
        }
        vm.prank(MANAGER);
        account.installKeyRing(ring);

        bytes32 expected = keyRingHashOf(ring);
        assertEq(account.KEYRING_HASH(), expected);
        for (uint256 i; i < MAX_KEYRING_SIZE; ++i) {
            assertEq(account.sessionKeyRing(ring[i].verifier), expected, "member missing");
        }
    }

    function test_installKeyRing_revertsAboveMaxSize() public {
        uint256 oversize = uint256(MAX_KEYRING_SIZE) + 1;
        WorldChainAccountVerifier[] memory ring = new WorldChainAccountVerifier[](oversize);
        for (uint256 i; i < oversize; ++i) {
            ring[i] = descriptor(address(happyVerifier));
        }

        vm.expectRevert(abi.encodeWithSelector(KeyringStore.InvalidKeyRingSize.selector, oversize));
        vm.prank(MANAGER);
        account.installKeyRing(ring);
    }

    function test_installKeyRing_replacesPreviousRing() public {
        installSingleSessionKeyRing(account, address(happyVerifier));
        bytes32 firstHash = account.KEYRING_HASH();
        assertEq(account.sessionKeyRing(address(happyVerifier)), firstHash);

        // Replace with a brand new ring that excludes the old member.
        WorldChainAccountVerifier[] memory replacement = new WorldChainAccountVerifier[](1);
        replacement[0] = descriptor(address(secondHappyVerifier));
        vm.prank(MANAGER);
        account.installKeyRing(replacement);

        bytes32 secondHash = account.KEYRING_HASH();
        assertTrue(firstHash != secondHash, "keyRingHash MUST rotate");
        // Old member's marker is now stale and no longer equals the active hash.
        bytes32 staleMarker = account.sessionKeyRing(address(happyVerifier));
        assertEq(staleMarker, firstHash, "old marker is still firstHash (mapping not wiped)");
        assertTrue(staleMarker != secondHash, "old member is NOT a member of the new ring");
        // New member is installed.
        assertEq(account.sessionKeyRing(address(secondHappyVerifier)), secondHash);
    }

    function test_installKeyRing_emptyRingProducesNonzeroHash() public {
        WorldChainAccountVerifier[] memory ring = new WorldChainAccountVerifier[](0);

        vm.prank(MANAGER);
        account.installKeyRing(ring);

        bytes32 expected = keyRingHashOf(ring);
        assertTrue(expected != bytes32(0), "empty ABI-encoded ring has a non-zero hash");
        assertEq(account.KEYRING_HASH(), expected);
        // No verifier is a member of an empty ring.
        assertEq(account.sessionKeyRing(address(happyVerifier)), bytes32(0));
    }

    function test_installKeyRing_bubblesHookRevert() public {
        WorldChainAccountVerifier[] memory ring = new WorldChainAccountVerifier[](1);
        ring[0] = descriptorWithData(address(revertingInstallVerifier), bytes("boom"));

        vm.expectRevert(abi.encodeWithSelector(MockRevertingInstallVerifier.InstallReverted.selector, bytes("boom")));
        vm.prank(MANAGER);
        account.installKeyRing(ring);

        // Failed install MUST unwind — keyRingHash stays zero.
        assertEq(account.KEYRING_HASH(), bytes32(0));
        assertEq(account.sessionKeyRing(address(revertingInstallVerifier)), bytes32(0));
    }

    function test_installKeyRing_runsHookInAccountStorage() public {
        WorldChainAccountVerifier[] memory ring = new WorldChainAccountVerifier[](1);
        ring[0] = descriptor(address(storageWriter));

        vm.prank(MANAGER);
        account.installKeyRing(ring);

        bytes32 onAccount = vm.load(address(account), storageWriter.SENTINEL_SLOT());
        bytes32 onVerifier = vm.load(address(storageWriter), storageWriter.SENTINEL_SLOT());
        assertEq(onAccount, storageWriter.SENTINEL_VALUE(), "hook MUST write into account storage");
        assertEq(onVerifier, bytes32(0), "hook MUST NOT touch verifier storage");
    }

    function test_installKeyRing_fuzz_sizes(uint8 size) public {
        size = uint8(bound(size, 0, MAX_KEYRING_SIZE));
        WorldChainAccountVerifier[] memory ring = new WorldChainAccountVerifier[](size);
        for (uint256 i; i < size; ++i) {
            ring[i] = descriptor(address(new MockHappyVerifier()));
        }
        vm.prank(MANAGER);
        account.installKeyRing(ring);
        assertEq(account.KEYRING_HASH(), keyRingHashOf(ring));
    }

    // ─── isValidSignatureForAdmin ─────────────────────────────────────────────

    function test_isValidSignatureForAdmin_returnsMagic() public {
        installAdminAs(account, address(happyVerifier));
        bytes4 magic = account.isValidSignatureForAdmin(bytes32(uint256(1)), hex"");
        assertEq(magic, ERC1271_MAGIC);
    }

    function test_isValidSignatureForAdmin_revertsWhenAdminUninstalled() public {
        vm.expectRevert(IWorldChainAccountRouterErrors.AdminNotInstalled.selector);
        account.isValidSignatureForAdmin(bytes32(uint256(1)), hex"");
    }

    function test_isValidSignatureForAdmin_bubblesVerifierRevert() public {
        installAdminAs(account, address(revertingSignatureVerifier));
        vm.expectRevert(MockRevertingSignatureVerifier.SignatureCheckReverted.selector);
        account.isValidSignatureForAdmin(bytes32(uint256(1)), hex"");
    }

    // ─── isValidSignatureForVerifier ──────────────────────────────────────────

    function test_isValidSignatureForVerifier_returnsMagicForInstalledMember() public {
        installSingleSessionKeyRing(account, address(happyVerifier));
        bytes4 magic = account.isValidSignatureForVerifier(address(happyVerifier), bytes32(uint256(7)), hex"");
        assertEq(magic, ERC1271_MAGIC);
    }

    function test_isValidSignatureForVerifier_revertsForUnknownVerifier() public {
        installSingleSessionKeyRing(account, address(happyVerifier));
        vm.expectRevert(
            abi.encodeWithSelector(
                IWorldChainAccountRouterErrors.VerifierNotInstalled.selector, address(rejectingVerifier)
            )
        );
        account.isValidSignatureForVerifier(address(rejectingVerifier), bytes32(uint256(7)), hex"");
    }

    function test_isValidSignatureForVerifier_revertsWhenNoKeyRing() public {
        // No keyring installed → keyRingHash == 0, modifier MUST reject every verifier.
        vm.expectRevert(
            abi.encodeWithSelector(IWorldChainAccountRouterErrors.VerifierNotInstalled.selector, address(happyVerifier))
        );
        account.isValidSignatureForVerifier(address(happyVerifier), bytes32(uint256(7)), hex"");
    }

    function test_isValidSignatureForVerifier_revertsForStaleMemberAfterRotation() public {
        installSingleSessionKeyRing(account, address(happyVerifier));
        // Rotate to a new ring that excludes happyVerifier.
        WorldChainAccountVerifier[] memory replacement = new WorldChainAccountVerifier[](1);
        replacement[0] = descriptor(address(secondHappyVerifier));
        vm.prank(MANAGER);
        account.installKeyRing(replacement);

        vm.expectRevert(
            abi.encodeWithSelector(IWorldChainAccountRouterErrors.VerifierNotInstalled.selector, address(happyVerifier))
        );
        account.isValidSignatureForVerifier(address(happyVerifier), bytes32(uint256(7)), hex"");
    }

    function test_isValidSignatureForVerifier_revertsForZeroVerifier() public {
        installSingleSessionKeyRing(account, address(happyVerifier));
        vm.expectRevert(
            abi.encodeWithSelector(IWorldChainAccountRouterErrors.VerifierNotInstalled.selector, address(0))
        );
        account.isValidSignatureForVerifier(address(0), bytes32(0), hex"");
    }

    function test_isValidSignatureForVerifier_bubblesVerifierRevert() public {
        installSingleSessionKeyRing(account, address(revertingSignatureVerifier));
        vm.expectRevert(MockRevertingSignatureVerifier.SignatureCheckReverted.selector);
        account.isValidSignatureForVerifier(address(revertingSignatureVerifier), bytes32(0), hex"");
    }

    // ─── evaluateSessionPolicyForVerifier ─────────────────────────────────────

    function test_evaluateSessionPolicy_allowsForHappyVerifier() public {
        installSingleSessionKeyRing(account, address(happyVerifier));
        IWorldChainSessionVerifier.ExecutionTraceContext memory ctx = emptyContext();
        assertTrue(account.evaluateSessionPolicyForVerifier(address(happyVerifier), ctx));
    }

    function test_evaluateSessionPolicy_deniesForRejectingVerifier() public {
        installSingleSessionKeyRing(account, address(rejectingVerifier));
        IWorldChainSessionVerifier.ExecutionTraceContext memory ctx = emptyContext();
        assertFalse(account.evaluateSessionPolicyForVerifier(address(rejectingVerifier), ctx));
    }

    function test_evaluateSessionPolicy_revertsForUnknownVerifier() public {
        installSingleSessionKeyRing(account, address(happyVerifier));
        IWorldChainSessionVerifier.ExecutionTraceContext memory ctx = emptyContext();
        vm.expectRevert(
            abi.encodeWithSelector(
                IWorldChainAccountRouterErrors.VerifierNotInstalled.selector, address(rejectingVerifier)
            )
        );
        account.evaluateSessionPolicyForVerifier(address(rejectingVerifier), ctx);
    }

    // ─── Dispatch fallback ────────────────────────────────────────────────────

    function test_dispatch_unknownSelectorReverts() public {
        bytes4 sel = bytes4(keccak256("doesNotExist()"));
        vm.expectRevert(abi.encodeWithSelector(IWorldChainAccountRouterErrors.UnknownSelector.selector, sel));
        (bool ok,) = address(account).call(abi.encodePacked(sel));
        // The low-level `call` returns `false` to the test contract — the assertion is implicit:
        // `expectRevert` MUST have matched.
        assertFalse(ok);
    }

    function test_dispatch_shortCalldataRoutedAsRightPadded() public {
        // 1-byte calldata is right-padded to a selector of `0xff000000`, per Solidity's convention.
        bytes4 paddedSelector = bytes4(0xff000000);
        vm.expectRevert(abi.encodeWithSelector(IWorldChainAccountRouterErrors.UnknownSelector.selector, paddedSelector));
        (bool ok,) = address(account).call(hex"ff");
        assertFalse(ok);
    }

    function test_dispatch_emptyCalldataAcceptsValue() public {
        vm.deal(address(this), 1 ether);
        uint256 startingBalance = address(account).balance;
        (bool ok,) = address(account).call{value: 0.5 ether}("");
        assertTrue(ok, "empty-calldata transfer MUST succeed through receive()");
        assertEq(address(account).balance, startingBalance + 0.5 ether);
    }

    function test_dispatch_fallbackAcceptsValue() public {
        bytes4 sel = bytes4(keccak256("doesNotExist()"));
        // Fallback is payable but reverts on unknown selector. Value transfer is rolled back.
        uint256 startingBalance = address(account).balance;
        vm.deal(address(this), 1 ether);
        vm.expectRevert(abi.encodeWithSelector(IWorldChainAccountRouterErrors.UnknownSelector.selector, sel));
        (bool ok,) = address(account).call{value: 0.25 ether}(abi.encodePacked(sel));
        assertFalse(ok);
        assertEq(address(account).balance, startingBalance, "reverted call MUST not change balance");
    }

    // ─── Storage layout ───────────────────────────────────────────────────────

    function test_storage_keyringSlotIsErc7201() public {
        // Storage location MUST equal the EIP-7201 derivation from the documented label.
        bytes32 expected =
            keccak256(abi.encode(uint256(keccak256("worldchain.account.keyring")) - 1)) & ~bytes32(uint256(0xff));
        installAdminAs(account, address(happyVerifier));

        // The admin verifier address lives at slot `expected` (first field of the struct).
        bytes32 raw = vm.load(address(account), expected);
        assertEq(address(uint160(uint256(raw))), address(happyVerifier));
    }

    function test_storage_perAccountIsolation() public {
        IWorldChainAccount second = newAccountProxy();
        installAdminAs(account, address(happyVerifier));

        // The second account still has no admin — admin storage is per-proxy.
        assertEq(second.ADMIN_VERIFIER(), address(0));

        installAdminAs(second, address(secondHappyVerifier));
        assertEq(second.ADMIN_VERIFIER(), address(secondHappyVerifier));
        assertEq(account.ADMIN_VERIFIER(), address(happyVerifier), "first account untouched");
    }

    // ─── Fuzz: constructor & install paths ────────────────────────────────────

    function testFuzz_constructor_anyNonZeroManager(address manager) public {
        vm.assume(manager != address(0));
        WorldChainAccount impl = new WorldChainAccount(manager);
        assertEq(impl.MANAGER(), manager);
        assertEq(impl.VERSION(), 1);
    }

    function testFuzz_installAdmin_anyNonZeroVerifier(address verifier, bytes memory installation) public {
        _assumeEtchable(verifier);
        // Etch the happy-verifier runtime at the fuzzed address so the install hook delegatecall
        // can resolve a contract — any non-zero verifier must be acceptable from the router's POV.
        vm.etch(verifier, address(happyVerifier).code);

        vm.prank(MANAGER);
        account.installAdmin(descriptorWithData(verifier, installation));
        assertEq(account.ADMIN_VERIFIER(), verifier);

        // The witness in the account's storage matches the fuzzed installation payload, proving
        // the install hook ran via DELEGATECALL with the fuzzed bytes.
        bytes32 witness = vm.load(address(account), MockVerifierStorage.INSTALL_EVIDENCE_SLOT);
        assertEq(witness, keccak256(installation));
    }

    function testFuzz_installAdmin_revertsForAnyNonManagerCaller(address caller) public {
        vm.assume(caller != MANAGER);
        vm.expectRevert(IWorldChainAccountRouterErrors.CallerNotManager.selector);
        vm.prank(caller);
        account.installAdmin(descriptor(address(happyVerifier)));
    }

    function testFuzz_installKeyRing_revertsForAnyNonManagerCaller(address caller) public {
        vm.assume(caller != MANAGER);
        WorldChainAccountVerifier[] memory ring = new WorldChainAccountVerifier[](1);
        ring[0] = descriptor(address(happyVerifier));
        vm.expectRevert(IWorldChainAccountRouterErrors.CallerNotManager.selector);
        vm.prank(caller);
        account.installKeyRing(ring);
    }

    function testFuzz_installKeyRing_hashIsKeccakOfEncoding(uint8 size, bytes32 entropy) public {
        size = uint8(bound(size, 1, MAX_KEYRING_SIZE));
        WorldChainAccountVerifier[] memory ring = new WorldChainAccountVerifier[](size);
        bytes memory verifierCode = address(happyVerifier).code;
        for (uint256 i; i < size; ++i) {
            // Derive a unique pseudo-random etchable address per slot from the fuzzed entropy.
            address derived;
            for (uint256 tweak; tweak < 32; ++tweak) {
                address candidate = address(uint160(uint256(keccak256(abi.encode(entropy, i, tweak)))));
                if (_isEtchable(candidate)) {
                    derived = candidate;
                    break;
                }
            }
            require(derived != address(0), "fuzz entropy degenerate");
            vm.etch(derived, verifierCode);
            ring[i] = descriptorWithData(derived, abi.encode(entropy, i));
        }

        vm.prank(MANAGER);
        account.installKeyRing(ring);

        bytes32 expected = keccak256(abi.encode(ring));
        assertEq(account.KEYRING_HASH(), expected);
        for (uint256 i; i < size; ++i) {
            assertEq(account.sessionKeyRing(ring[i].verifier), expected, "missing member");
        }
    }

    // ─── Fuzz: dispatch helpers ───────────────────────────────────────────────

    function testFuzz_isValidSignatureForAdmin_preservesMagic(bytes32 hash, bytes memory signature) public {
        installAdminAs(account, address(happyVerifier));
        assertEq(account.isValidSignatureForAdmin(hash, signature), ERC1271_MAGIC);
    }

    function testFuzz_isValidSignatureForVerifier_preservesMagic(bytes32 hash, bytes memory signature) public {
        installSingleSessionKeyRing(account, address(happyVerifier));
        assertEq(account.isValidSignatureForVerifier(address(happyVerifier), hash, signature), ERC1271_MAGIC);
    }

    function testFuzz_isValidSignatureForVerifier_revertsForAnyNonMember(address probe) public {
        installSingleSessionKeyRing(account, address(happyVerifier));
        vm.assume(probe != address(happyVerifier));
        vm.expectRevert(abi.encodeWithSelector(IWorldChainAccountRouterErrors.VerifierNotInstalled.selector, probe));
        account.isValidSignatureForVerifier(probe, bytes32(0), hex"");
    }

    /// @notice The dispatch fallback must revert for every non-system selector. We pin the
    ///         expected revert via the leading 4 bytes of returndata only (the `UnknownSelector`
    ///         error selector) rather than the embedded byte4 payload, because the byte4 payload
    ///         depends on `Dispatch.sol`'s inline-assembly bytes4 alignment — a property fully
    ///         exercised by `test_dispatch_unknownSelectorReverts` and out of scope here.
    function testFuzz_dispatch_unknownSelectorReverts(bytes4 selector) public {
        // Filter out the router's known external selectors — those are valid entry points and
        // would not route through the fallback.
        vm.assume(selector != IWorldChainAccount.VERSION.selector);
        vm.assume(selector != IWorldChainAccount.MANAGER.selector);
        vm.assume(selector != IWorldChainAccount.ADMIN_VERIFIER.selector);
        vm.assume(selector != IWorldChainAccount.KEYRING_HASH.selector);
        vm.assume(selector != IWorldChainAccount.sessionKeyRing.selector);
        vm.assume(selector != IWorldChainAccount.installAdmin.selector);
        vm.assume(selector != IWorldChainAccount.installKeyRing.selector);
        vm.assume(selector != IWorldChainAccount.isValidSignatureForAdmin.selector);
        vm.assume(selector != IWorldChainAccount.isValidSignatureForVerifier.selector);
        vm.assume(selector != IWorldChainAccount.evaluateSessionPolicyForVerifier.selector);

        (bool ok, bytes memory ret) = address(account).call(abi.encodePacked(selector));
        assertFalse(ok, "fallback MUST revert");
        // Leading 4 bytes of revert data are the `UnknownSelector` error selector.
        assertTrue(ret.length >= 4, "returndata MUST carry the error selector");
        bytes4 errorSelector;
        assembly ("memory-safe") {
            errorSelector := mload(add(ret, 0x20))
        }
        assertEq(errorSelector, IWorldChainAccountRouterErrors.UnknownSelector.selector);
    }

    // ─── Helpers ──────────────────────────────────────────────────────────────

    function emptyContext() internal pure returns (IWorldChainSessionVerifier.ExecutionTraceContext memory ctx) {
        // All fields default-initialized; only enough to satisfy ABI decoding of the struct.
    }

    /// @dev Returns true iff `addr` is safe to `vm.etch` into without corrupting the test harness
    ///      (rules out the zero address, EVM precompiles, forge cheatcode addresses, and the
    ///      contracts we provisioned in `setUp`).
    function _isEtchable(address addr) internal view returns (bool) {
        if (addr == address(0)) return false;
        if (uint160(addr) <= 0xff) return false; // precompiles
        if (addr == BEACON_PREDEPLOY) return false;
        if (addr == address(accountImpl)) return false;
        if (addr == address(account)) return false;
        if (addr == address(beacon)) return false;
        if (addr == address(this)) return false;
        if (addr == address(vm)) return false;
        if (addr == address(happyVerifier)) return false;
        if (addr == address(secondHappyVerifier)) return false;
        if (addr == address(rejectingVerifier)) return false;
        if (addr == address(revertingInstallVerifier)) return false;
        if (addr == address(revertingSignatureVerifier)) return false;
        if (addr == address(reentrantAdminInstaller)) return false;
        if (addr == address(storageWriter)) return false;
        return true;
    }

    function _assumeEtchable(address addr) internal view {
        vm.assume(_isEtchable(addr));
    }
}
