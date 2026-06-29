// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {Vm} from "forge-std/Vm.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {NitroAttestationVerifier} from "../../src/proofs/nitro/NitroAttestationVerifier.sol";
import {CertManager} from "@nitro-validator/CertManager.sol";
import {ICertManager} from "@nitro-validator/ICertManager.sol";

/// @dev Test harness exposing the internal freshness check so we can exercise
///      both the stale and the future-dated branches without constructing a
///      full Nitro attestation document.
contract NitroAttestationVerifierHarness is NitroAttestationVerifier {
    constructor(ICertManager cm, address owner_) NitroAttestationVerifier(cm, owner_) {}

    function checkFreshness(uint64 timestampMs) external view {
        _checkFreshness(timestampMs);
    }
}

/// @dev Smoke tests for {NitroAttestationVerifier}. The full happy-path
///      verification of a real Nitro attestation document requires a fresh,
///      signed AWS NSM document and the full intermediate cert bundle, which
///      is not practical to check in via fixtures. The underlying
///      parser/cert-chain/P-384 verification logic is exhaustively tested
///      upstream in Base's `nitro-validator` repo (added as a Forge
///      submodule).
contract NitroAttestationVerifierTest is Test {
    CertManager certManager;
    NitroAttestationVerifier verifier;
    NitroAttestationVerifierHarness harness;

    address owner = makeAddr("owner");
    address attacker = makeAddr("attacker");

    bytes32 constant PCR0_A = bytes32(uint256(0xa0));
    bytes32 constant PCR1_A = bytes32(uint256(0xa1));
    bytes32 constant PCR2_A = bytes32(uint256(0xa2));

    bytes32 constant PCR0_B = bytes32(uint256(0xb0));
    bytes32 constant PCR1_B = bytes32(uint256(0xb1));
    bytes32 constant PCR2_B = bytes32(uint256(0xb2));

    function setUp() public {
        certManager = new CertManager();

        verifier = new NitroAttestationVerifier(ICertManager(address(certManager)), owner);
        harness = new NitroAttestationVerifierHarness(ICertManager(address(certManager)), owner);

        // The allowlist is empty at deploy; pre-approve PCR set A for the
        // suite, leaving PCR set B unapproved for negative-path tests.
        vm.prank(owner);
        verifier.approvePCRSet(PCR0_A, PCR1_A, PCR2_A);
    }

    /*//////////////////////////////////////////////////////////////
                              CONSTRUCTOR
    //////////////////////////////////////////////////////////////*/

    function test_Constructor_WiresCertManager() public view {
        assertEq(address(verifier.certManager()), address(certManager));
    }

    function test_Constructor_SetsOwner() public view {
        assertEq(verifier.owner(), owner);
    }

    function test_Constructor_DeploysWithEmptyAllowlist() public {
        NitroAttestationVerifier v = new NitroAttestationVerifier(ICertManager(address(certManager)), owner);
        assertFalse(v.isPCRSetApproved(PCR0_A, PCR1_A, PCR2_A));
        assertFalse(v.isPCRSetApproved(PCR0_B, PCR1_B, PCR2_B));
    }

    function test_SetupApprovedPcrSetA() public view {
        // setUp pre-approves PCR set A; sanity-check it.
        assertTrue(verifier.isPCRSetApproved(PCR0_A, PCR1_A, PCR2_A));
        assertFalse(verifier.isPCRSetApproved(PCR0_B, PCR1_B, PCR2_B));
    }

    /*//////////////////////////////////////////////////////////////
                             ALLOWLIST OPS
    //////////////////////////////////////////////////////////////*/

    function test_ApprovePCRSet_OnlyOwner() public {
        vm.prank(attacker);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, attacker));
        verifier.approvePCRSet(PCR0_B, PCR1_B, PCR2_B);
    }

    function test_ApprovePCRSet_AddsAndEmits() public {
        vm.prank(owner);
        vm.expectEmit(false, false, false, true);
        emit NitroAttestationVerifier.PCRSetApproved(PCR0_B, PCR1_B, PCR2_B);
        verifier.approvePCRSet(PCR0_B, PCR1_B, PCR2_B);
        assertTrue(verifier.isPCRSetApproved(PCR0_B, PCR1_B, PCR2_B));
    }

    function test_ApprovePCRSet_IdempotentNoEvent() public {
        // PCR0_A/1/2 are already approved at deploy.
        vm.recordLogs();
        vm.prank(owner);
        verifier.approvePCRSet(PCR0_A, PCR1_A, PCR2_A);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(logs.length, 0);
        assertTrue(verifier.isPCRSetApproved(PCR0_A, PCR1_A, PCR2_A));
    }

    function test_RevokePCRSet_OnlyOwner() public {
        vm.prank(attacker);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, attacker));
        verifier.revokePCRSet(PCR0_A, PCR1_A, PCR2_A);
    }

    function test_RevokePCRSet_RemovesAndEmits() public {
        vm.prank(owner);
        vm.expectEmit(false, false, false, true);
        emit NitroAttestationVerifier.PCRSetRevoked(PCR0_A, PCR1_A, PCR2_A);
        verifier.revokePCRSet(PCR0_A, PCR1_A, PCR2_A);
        assertFalse(verifier.isPCRSetApproved(PCR0_A, PCR1_A, PCR2_A));
    }

    function test_RevokePCRSet_NoOpForUnknown() public {
        vm.recordLogs();
        vm.prank(owner);
        verifier.revokePCRSet(PCR0_B, PCR1_B, PCR2_B);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(logs.length, 0);
    }

    /*//////////////////////////////////////////////////////////////
                            VERIFICATION
    //////////////////////////////////////////////////////////////*/

    function test_VerifyAttestation_RevertsOnGarbageInput() public {
        // CBOR/cert-chain parse failures bubble up; we only check that random
        // bytes do not silently verify. Specific failure modes are tested
        // upstream in base/nitro-validator.
        vm.expectRevert();
        verifier.verifyAttestation(hex"00", hex"00");
    }

    /*//////////////////////////////////////////////////////////////
                              FRESHNESS
    //////////////////////////////////////////////////////////////*/

    function test_MaxAge_Is60Minutes() public view {
        assertEq(verifier.MAX_AGE(), 60 minutes);
    }

    function test_ClockSkewTolerance_Is5Minutes() public view {
        assertEq(verifier.CLOCK_SKEW_TOLERANCE(), 5 minutes);
    }

    function test_Freshness_AcceptsCurrentTimestamp() public {
        vm.warp(1_000_000);
        harness.checkFreshness(uint64(1_000_000 * 1000)); // milliseconds
    }

    function test_Freshness_RejectsStale() public {
        vm.warp(1_000_000);
        uint64 staleMs = uint64((1_000_000 - 3601) * 1000); // 1h + 1s in the past
        vm.expectRevert(abi.encodeWithSelector(NitroAttestationVerifier.AttestationStale.selector, 3601));
        harness.checkFreshness(staleMs);
    }

    function test_Freshness_AcceptsSlightlyFuture() public {
        vm.warp(1_000_000);
        uint64 futureMs = uint64((1_000_000 + 4 * 60) * 1000);
        harness.checkFreshness(futureMs);
    }

    function test_Freshness_RejectsFarFuture() public {
        vm.warp(1_000_000);
        uint64 futureMs = uint64((1_000_000 + 6 * 60) * 1000);
        vm.expectRevert(abi.encodeWithSelector(NitroAttestationVerifier.AttestationFromFuture.selector, 360));
        harness.checkFreshness(futureMs);
    }
}
