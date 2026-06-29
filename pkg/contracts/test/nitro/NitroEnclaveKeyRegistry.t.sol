// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {NitroEnclaveKeyRegistry} from "../../src/proofs/nitro/NitroEnclaveKeyRegistry.sol";
import {MockNitroAttestationVerifier} from "./mocks/MockNitroAttestationVerifier.sol";

contract NitroEnclaveKeyRegistryTest is Test {
    MockNitroAttestationVerifier attestationVerifier;
    NitroEnclaveKeyRegistry registry;

    address owner = makeAddr("owner");
    address attacker = makeAddr("attacker");

    bytes32 constant PCR0 = bytes32(uint256(0xa));
    bytes32 constant PCR1 = bytes32(uint256(0xb));
    bytes32 constant PCR2 = bytes32(uint256(0xc));

    bytes constant TBS = hex"01010101";
    bytes constant SIG = hex"02020202";

    bytes pubKey;
    bytes otherKey;

    function setUp() public {
        attestationVerifier = new MockNitroAttestationVerifier();
        registry = new NitroEnclaveKeyRegistry(attestationVerifier, owner);

        pubKey = _key(0x01);
        otherKey = _key(0xAB);
        attestationVerifier.setExpectation(TBS, SIG, pubKey, PCR0, PCR1, PCR2);
    }

    function _key(uint8 seed) internal pure returns (bytes memory key) {
        key = new bytes(65);
        key[0] = 0x04;
        for (uint256 i = 1; i < 65; i++) {
            key[i] = bytes1(seed + uint8(i));
        }
    }

    function test_RegisterKey_StoresKeyAndEmits() public {
        vm.expectEmit(false, false, false, true);
        emit NitroEnclaveKeyRegistry.KeyRegistered(pubKey, PCR0, PCR1, PCR2);
        registry.registerKey(TBS, SIG);

        assertTrue(registry.isKeyRegistered(pubKey));
    }

    function test_RegisterKey_RevertsWhenVerifierRejects() public {
        // No expectation set for these (TBS, SIG).
        vm.expectRevert(MockNitroAttestationVerifier.UnexpectedCall.selector);
        registry.registerKey(hex"deadbeef", SIG);
    }

    function test_RegisterKey_RevertsOnMalformedKey() public {
        bytes memory badKey = hex"0301";
        attestationVerifier.setExpectation(hex"badf00", SIG, badKey, PCR0, PCR1, PCR2);
        vm.expectRevert(NitroEnclaveKeyRegistry.InvalidPublicKey.selector);
        registry.registerKey(hex"badf00", SIG);
    }

    function test_IsKeyRegistered_FalseForUnknown() public view {
        assertFalse(registry.isKeyRegistered(otherKey));
    }

    function test_RevokeKey_OnlyOwner() public {
        registry.registerKey(TBS, SIG);

        vm.prank(attacker);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, attacker));
        registry.revokeKey(pubKey);

        vm.prank(owner);
        vm.expectEmit(false, false, false, true);
        emit NitroEnclaveKeyRegistry.KeyRevoked(pubKey);
        registry.revokeKey(pubKey);

        assertFalse(registry.isKeyRegistered(pubKey));
    }

    function test_RevokeKey_RevertsForUnregistered() public {
        vm.prank(owner);
        vm.expectRevert(NitroEnclaveKeyRegistry.KeyNotRegistered.selector);
        registry.revokeKey(pubKey);
    }

    function test_MultipleInstancesSameImage() public {
        // Two validator instances run the same enclave image → same PCR triple,
        // but each has its own ephemeral key. Both must register successfully
        // and both must be queryable via isKeyRegistered. (No on-chain
        // PCRs → key index exists; the KeyRegistered events are the off-chain
        // source of truth.)
        registry.registerKey(TBS, SIG);

        bytes memory tbs2 = hex"cafe";
        attestationVerifier.setExpectation(tbs2, SIG, otherKey, PCR0, PCR1, PCR2);
        registry.registerKey(tbs2, SIG);

        assertTrue(registry.isKeyRegistered(pubKey));
        assertTrue(registry.isKeyRegistered(otherKey));
    }

    function test_Constructor_SetsOwnerAndVerifier() public view {
        assertEq(registry.owner(), owner);
        assertEq(address(registry.verifier()), address(attestationVerifier));
    }

    function test_Constructor_RevertsOnZeroOwner() public {
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableInvalidOwner.selector, address(0)));
        new NitroEnclaveKeyRegistry(attestationVerifier, address(0));
    }

    function test_RegisterKey_ReturnsKeyAndPCRs() public {
        // The function's return values must be forwarded verbatim from the
        // verifier so off-chain consumers can rely on them.
        (bytes memory key, bytes32 p0, bytes32 p1, bytes32 p2) = registry.registerKey(TBS, SIG);
        assertEq(key, pubKey);
        assertEq(p0, PCR0);
        assertEq(p1, PCR1);
        assertEq(p2, PCR2);
    }

    function test_RegisterKey_RejectsValidLengthKeyWithWrongPrefix() public {
        // 65-byte key but the SEC1 prefix is 0x03 (compressed-y-odd) instead
        // of 0x04 (uncompressed). The registry's defensive check must catch
        // this even though the real verifier already enforces 0x04.
        bytes memory badKey = new bytes(65);
        badKey[0] = 0x03;
        for (uint256 i = 1; i < 65; i++) {
            badKey[i] = bytes1(uint8(i));
        }
        attestationVerifier.setExpectation(hex"feedface", SIG, badKey, PCR0, PCR1, PCR2);
        vm.expectRevert(NitroEnclaveKeyRegistry.InvalidPublicKey.selector);
        registry.registerKey(hex"feedface", SIG);
    }

    function test_RegisterKey_RejectsKeyWithLength64() public {
        // SEC1-uncompressed minus the 0x04 prefix — wrong length, must revert.
        bytes memory key64 = new bytes(64);
        for (uint256 i = 0; i < 64; i++) {
            key64[i] = bytes1(uint8(0xAA));
        }
        attestationVerifier.setExpectation(hex"6464", SIG, key64, PCR0, PCR1, PCR2);
        vm.expectRevert(NitroEnclaveKeyRegistry.InvalidPublicKey.selector);
        registry.registerKey(hex"6464", SIG);
    }

    function test_RegisterKey_RejectsKeyWithLength66() public {
        bytes memory key66 = new bytes(66);
        key66[0] = 0x04;
        attestationVerifier.setExpectation(hex"6666", SIG, key66, PCR0, PCR1, PCR2);
        vm.expectRevert(NitroEnclaveKeyRegistry.InvalidPublicKey.selector);
        registry.registerKey(hex"6666", SIG);
    }

    function test_RevokeKey_AlreadyRevokedRevertsKeyNotRegistered() public {
        registry.registerKey(TBS, SIG);
        vm.prank(owner);
        registry.revokeKey(pubKey);
        // A second revoke for the same (now Revoked) key must surface
        // `KeyNotRegistered`, NOT silently succeed and re-emit the event.
        vm.prank(owner);
        vm.expectRevert(NitroEnclaveKeyRegistry.KeyNotRegistered.selector);
        registry.revokeKey(pubKey);
    }

    function test_RevokeKey_RevertsForUnknownKey() public {
        // Revoking a never-seen key must surface `KeyNotRegistered`.
        vm.prank(owner);
        vm.expectRevert(NitroEnclaveKeyRegistry.KeyNotRegistered.selector);
        registry.revokeKey(otherKey);
    }

    function test_RegisterKey_EmitsKeyRegisteredWithExactPCRs() public {
        // Stronger assertion: emits the exact event including all PCRs.
        vm.expectEmit(false, false, false, true);
        emit NitroEnclaveKeyRegistry.KeyRegistered(pubKey, PCR0, PCR1, PCR2);
        registry.registerKey(TBS, SIG);
    }

    function test_KeyStatus_ForUnknownKeyIsZero() public view {
        // The default `KeyStatus.Unknown == 0` invariant: any never-seen key
        // must report Unknown, isKeyRegistered=false, isKeyRevoked=false.
        assertEq(uint8(registry.keyStatus(otherKey)), uint8(NitroEnclaveKeyRegistry.KeyStatus.Unknown));
        assertFalse(registry.isKeyRegistered(otherKey));
        assertFalse(registry.isKeyRevoked(otherKey));
    }

    function test_RegisterKey_PropagatesVerifierRevertVerbatim() public {
        // The verifier rejects; the registry must not swallow the revert.
        // (Already covered by `test_RegisterKey_RevertsWhenVerifierRejects`,
        // but here we lock in the exact selector so a future refactor that
        // wraps the call in a try/catch can't accidentally turn it into
        // a different error type.)
        vm.expectRevert(MockNitroAttestationVerifier.UnexpectedCall.selector);
        registry.registerKey(hex"deadbeef", hex"00");
    }

    function test_RevokeKey_PreventsReregistration() public {
        registry.registerKey(TBS, SIG);

        vm.prank(owner);
        registry.revokeKey(pubKey);
        assertFalse(registry.isKeyRegistered(pubKey));
        assertTrue(registry.isKeyRevoked(pubKey));

        // Anyone re-submitting the same attestation must fail.
        vm.expectRevert(NitroEnclaveKeyRegistry.KeyRevokedPermanently.selector);
        registry.registerKey(TBS, SIG);

        assertFalse(registry.isKeyRegistered(pubKey));
    }

    function test_RevokeKey_AlsoBlocksRegistrationUnderDifferentPCRs() public {
        registry.registerKey(TBS, SIG);

        vm.prank(owner);
        registry.revokeKey(pubKey);

        // Even if a doc later asserted the same key under different PCRs, the
        // revoke must be sticky on the key itself.
        bytes32 otherPcr0 = bytes32(uint256(0xff));
        attestationVerifier.setExpectation(hex"1234", SIG, pubKey, otherPcr0, PCR1, PCR2);
        vm.expectRevert(NitroEnclaveKeyRegistry.KeyRevokedPermanently.selector);
        registry.registerKey(hex"1234", SIG);
    }

    function test_IsKeyRevoked_FalseBeforeRevoke() public {
        registry.registerKey(TBS, SIG);
        assertFalse(registry.isKeyRevoked(pubKey));
    }

    function test_RegisterKey_RevertsIfAlreadyActive() public {
        registry.registerKey(TBS, SIG);
        vm.expectRevert(NitroEnclaveKeyRegistry.KeyAlreadyRegistered.selector);
        registry.registerKey(TBS, SIG);
    }

    function test_KeyStatus_LifecycleTransitions() public {
        // Unknown
        assertEq(uint8(registry.keyStatus(pubKey)), uint8(NitroEnclaveKeyRegistry.KeyStatus.Unknown));

        // Active
        registry.registerKey(TBS, SIG);
        assertEq(uint8(registry.keyStatus(pubKey)), uint8(NitroEnclaveKeyRegistry.KeyStatus.Active));

        // Revoked
        vm.prank(owner);
        registry.revokeKey(pubKey);
        assertEq(uint8(registry.keyStatus(pubKey)), uint8(NitroEnclaveKeyRegistry.KeyStatus.Revoked));
    }

    function test_MultiImageCoexistence() public {
        // Image A.
        registry.registerKey(TBS, SIG);

        // Image B: different PCR triple, different key, different attestation.
        bytes32 pcr0B = bytes32(uint256(0x10));
        bytes32 pcr1B = bytes32(uint256(0x11));
        bytes32 pcr2B = bytes32(uint256(0x12));
        bytes memory tbsB = hex"03030303";
        attestationVerifier.setExpectation(tbsB, SIG, otherKey, pcr0B, pcr1B, pcr2B);
        registry.registerKey(tbsB, SIG);

        // Both keys are registered.
        assertTrue(registry.isKeyRegistered(pubKey));
        assertTrue(registry.isKeyRegistered(otherKey));
    }
}
