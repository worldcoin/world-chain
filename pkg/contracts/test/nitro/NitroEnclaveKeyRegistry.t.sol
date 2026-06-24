// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {NitroAttestationVerifier} from "../../src/proofs/nitro/NitroAttestationVerifier.sol";
import {NitroEnclaveKeyRegistry} from "../../src/proofs/nitro/NitroEnclaveKeyRegistry.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";

contract NitroEnclaveKeyRegistryTest is Test {
    NitroAttestationVerifier attestationVerifier;
    NitroEnclaveKeyRegistry registry;

    address owner = makeAddr("owner");
    address relayer = makeAddr("relayer");
    address attacker = makeAddr("attacker");

    bytes32 constant PCR0 = bytes32(uint256(0xa));
    bytes32 constant PCR1 = bytes32(uint256(0xb));
    bytes32 constant PCR2 = bytes32(uint256(0xc));

    bytes constant DOC = hex"deadbeef";

    bytes pubKey;
    bytes otherKey;

    function setUp() public {
        attestationVerifier = new NitroAttestationVerifier(owner, relayer);
        registry = new NitroEnclaveKeyRegistry(attestationVerifier, owner);

        pubKey = _key(0x01);
        otherKey = _key(0xAB);

        vm.prank(relayer);
        attestationVerifier.attestVerified(DOC, pubKey, PCR0, PCR1, PCR2);
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
        registry.registerKey(DOC, pubKey, PCR0, PCR1, PCR2);

        assertTrue(registry.isKeyRegistered(pubKey));
        assertEq(registry.getKeyByPCRs(PCR0, PCR1, PCR2), pubKey);
    }

    function test_RegisterKey_RevertsWhenAttestationRejected() public {
        // No relayer attestation for `otherKey`.
        vm.expectRevert(NitroEnclaveKeyRegistry.AttestationRejected.selector);
        registry.registerKey(DOC, otherKey, PCR0, PCR1, PCR2);
    }

    function test_RegisterKey_RevertsForEmptyKey() public {
        vm.expectRevert(NitroEnclaveKeyRegistry.EmptyPublicKey.selector);
        registry.registerKey(DOC, "", PCR0, PCR1, PCR2);
    }

    function test_IsKeyRegistered_FalseForUnknown() public view {
        assertFalse(registry.isKeyRegistered(otherKey));
    }

    function test_GetKeyByPCRs_EmptyForUnknown() public view {
        assertEq(registry.getKeyByPCRs(bytes32(0), bytes32(0), bytes32(0)), bytes(""));
    }

    function test_RevokeKey_OnlyOwner() public {
        registry.registerKey(DOC, pubKey, PCR0, PCR1, PCR2);

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
        registry.registerKey(DOC, pubKey, PCR0, PCR1, PCR2);

        // attest a rotated key for the same PCR triple via a new document
        bytes memory doc2 = hex"cafe";
        vm.prank(relayer);
        attestationVerifier.attestVerified(doc2, otherKey, PCR0, PCR1, PCR2);

        registry.registerKey(doc2, otherKey, PCR0, PCR1, PCR2);
        assertEq(registry.getKeyByPCRs(PCR0, PCR1, PCR2), otherKey);
        // Old key remains "registered" until explicitly revoked (documented behaviour).
        assertTrue(registry.isKeyRegistered(pubKey));
        assertTrue(registry.isKeyRegistered(otherKey));
    }
}
