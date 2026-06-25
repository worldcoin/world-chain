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
        attestationVerifier.setExpectation(TBS, SIG, PCR0, PCR1, PCR2, pubKey);
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
        registry.registerKey(TBS, SIG, PCR0, PCR1, PCR2);

        assertTrue(registry.isKeyRegistered(pubKey));
        assertEq(registry.getKeyByPCRs(PCR0, PCR1, PCR2), pubKey);
    }

    function test_RegisterKey_RevertsWhenVerifierRejects() public {
        // No expectation set for these PCRs.
        vm.expectRevert(MockNitroAttestationVerifier.UnexpectedCall.selector);
        registry.registerKey(TBS, SIG, bytes32(uint256(0xdead)), PCR1, PCR2);
    }

    function test_RegisterKey_RevertsOnMalformedKey() public {
        bytes memory badKey = hex"0301";
        attestationVerifier.setExpectation(hex"badf00", SIG, PCR0, PCR1, PCR2, badKey);
        vm.expectRevert(NitroEnclaveKeyRegistry.InvalidPublicKey.selector);
        registry.registerKey(hex"badf00", SIG, PCR0, PCR1, PCR2);
    }

    function test_IsKeyRegistered_FalseForUnknown() public view {
        assertFalse(registry.isKeyRegistered(otherKey));
    }

    function test_GetKeyByPCRs_EmptyForUnknown() public view {
        assertEq(registry.getKeyByPCRs(bytes32(0), bytes32(0), bytes32(0)), bytes(""));
    }

    function test_RevokeKey_OnlyOwner() public {
        registry.registerKey(TBS, SIG, PCR0, PCR1, PCR2);

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

    function test_Rotation_SupersedesPriorKeyForSamePCRs() public {
        registry.registerKey(TBS, SIG, PCR0, PCR1, PCR2);

        // Set up a rotated key with a new attestation for the same PCR triple.
        bytes memory tbs2 = hex"cafe";
        attestationVerifier.setExpectation(tbs2, SIG, PCR0, PCR1, PCR2, otherKey);

        registry.registerKey(tbs2, SIG, PCR0, PCR1, PCR2);
        assertEq(registry.getKeyByPCRs(PCR0, PCR1, PCR2), otherKey);
        // Old key stays registered until explicitly revoked.
        assertTrue(registry.isKeyRegistered(pubKey));
        assertTrue(registry.isKeyRegistered(otherKey));
    }

    function test_Constructor_SetsOwnerAndVerifier() public view {
        assertEq(registry.owner(), owner);
        assertEq(address(registry.verifier()), address(attestationVerifier));
    }
}
