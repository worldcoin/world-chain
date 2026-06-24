// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {NitroAttestationVerifier} from "../../src/proofs/nitro/NitroAttestationVerifier.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";

contract NitroAttestationVerifierTest is Test {
    NitroAttestationVerifier verifier;

    address owner = makeAddr("owner");
    address relayer = makeAddr("relayer");
    address otherRelayer = makeAddr("otherRelayer");
    address attacker = makeAddr("attacker");

    bytes32 constant PCR0 = bytes32(uint256(0xa));
    bytes32 constant PCR1 = bytes32(uint256(0xb));
    bytes32 constant PCR2 = bytes32(uint256(0xc));

    bytes constant DOC = hex"deadbeef";

    bytes pubKey;

    function setUp() public {
        verifier = new NitroAttestationVerifier(owner, relayer);

        // 65-byte SEC1-uncompressed dummy key.
        pubKey = new bytes(65);
        pubKey[0] = 0x04;
        for (uint256 i = 1; i < 65; i++) {
            pubKey[i] = bytes1(uint8(i));
        }
    }

    function test_Constructor_SetsRelayerAndOwner() public view {
        assertEq(verifier.owner(), owner);
        assertTrue(verifier.isRelayer(relayer));
        assertFalse(verifier.isRelayer(otherRelayer));
    }

    function test_SetRelayer_OnlyOwner() public {
        vm.prank(attacker);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, attacker));
        verifier.setRelayer(otherRelayer, true);

        vm.prank(owner);
        verifier.setRelayer(otherRelayer, true);
        assertTrue(verifier.isRelayer(otherRelayer));

        vm.prank(owner);
        verifier.setRelayer(relayer, false);
        assertFalse(verifier.isRelayer(relayer));
    }

    function test_AttestVerified_RecordsCommitment() public {
        vm.prank(relayer);
        verifier.attestVerified(DOC, pubKey, PCR0, PCR1, PCR2);

        assertTrue(verifier.verifyAttestation(DOC, pubKey, PCR0, PCR1, PCR2));
    }

    function test_AttestVerified_EmitsEvent() public {
        vm.prank(relayer);
        vm.expectEmit(true, false, false, true);
        emit NitroAttestationVerifier.AttestationVerified(keccak256(DOC), pubKey, PCR0, PCR1, PCR2);
        verifier.attestVerified(DOC, pubKey, PCR0, PCR1, PCR2);
    }

    function test_AttestVerified_RevertsForNonRelayer() public {
        vm.prank(attacker);
        vm.expectRevert(abi.encodeWithSelector(NitroAttestationVerifier.NotRelayer.selector, attacker));
        verifier.attestVerified(DOC, pubKey, PCR0, PCR1, PCR2);
    }

    function test_AttestVerified_RevertsForEmptyDoc() public {
        vm.prank(relayer);
        vm.expectRevert(NitroAttestationVerifier.EmptyAttestationDoc.selector);
        verifier.attestVerified("", pubKey, PCR0, PCR1, PCR2);
    }

    function test_AttestVerified_RevertsForBadPublicKeyLength() public {
        bytes memory badKey = new bytes(64);
        badKey[0] = 0x04;
        vm.prank(relayer);
        vm.expectRevert(NitroAttestationVerifier.InvalidPublicKey.selector);
        verifier.attestVerified(DOC, badKey, PCR0, PCR1, PCR2);
    }

    function test_AttestVerified_RevertsForBadPublicKeyPrefix() public {
        bytes memory badKey = new bytes(65);
        badKey[0] = 0x02; // compressed prefix, not uncompressed.
        vm.prank(relayer);
        vm.expectRevert(NitroAttestationVerifier.InvalidPublicKey.selector);
        verifier.attestVerified(DOC, badKey, PCR0, PCR1, PCR2);
    }

    function test_VerifyAttestation_FalseForMismatch() public {
        vm.prank(relayer);
        verifier.attestVerified(DOC, pubKey, PCR0, PCR1, PCR2);

        // wrong PCR
        assertFalse(verifier.verifyAttestation(DOC, pubKey, bytes32(uint256(0xdead)), PCR1, PCR2));
        // unattested doc
        assertFalse(verifier.verifyAttestation(hex"00", pubKey, PCR0, PCR1, PCR2));
    }
}
