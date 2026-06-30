// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {Vm} from "forge-std/Vm.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {NitroAttestationVerifier} from "../../src/proofs/nitro/NitroAttestationVerifier.sol";
import {CertManager} from "@nitro-validator/CertManager.sol";
import {ICertManager} from "@nitro-validator/ICertManager.sol";
import {NitroValidator} from "@nitro-validator/NitroValidator.sol";
import {CborElement, LibCborElement} from "@nitro-validator/CborDecode.sol";

/// @dev Test harness exposing internal hooks so we can exercise post-validation
///      branches (cabundle bound, PCR allowlist, embedded-key shape, freshness)
///      without synthesising a fresh AWS-signed Nitro attestation.
contract NitroAttestationVerifierHarness is NitroAttestationVerifier {
    constructor(ICertManager cm, address owner_) NitroAttestationVerifier(cm, owner_) {}

    function checkFreshness(uint64 timestampMs) external view {
        _checkFreshness(timestampMs);
    }

    /// @dev Direct entry point into the post-validation step.
    function exposeVerifyValidated(bytes calldata attestationTbs, Ptrs memory ptrs)
        external
        returns (bytes memory publicKey, bytes32 pcr0, bytes32 pcr1, bytes32 pcr2)
    {
        return _verifyValidated(attestationTbs, ptrs);
    }
}

/// @dev Smoke + branch tests for {NitroAttestationVerifier}.
///
///      Full happy-path verification is also exercised against the real
///      AWS-signed attestation document checked in upstream as a fixture
///      (see {test_VerifyAttestation_RealFixture_*}). The remaining
///      post-validation branches are exercised via the harness which lets us
///      inject a synthetic {NitroValidator.Ptrs} struct without having to
///      forge a full Nitro document + cert chain.
contract NitroAttestationVerifierTest is Test {
    using LibCborElement for CborElement;

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
        vm.startPrank(owner);
        verifier.approvePCRSet(PCR0_A, PCR1_A, PCR2_A);
        harness.approvePCRSet(PCR0_A, PCR1_A, PCR2_A);
        vm.stopPrank();
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

    function test_Constructor_RevertsOnZeroOwner() public {
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableInvalidOwner.selector, address(0)));
        new NitroAttestationVerifier(ICertManager(address(certManager)), address(0));
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

    function test_ApproveRevokeApprove_LifecycleEmitsEachTransition() public {
        // Revoke A.
        vm.prank(owner);
        vm.expectEmit(false, false, false, true);
        emit NitroAttestationVerifier.PCRSetRevoked(PCR0_A, PCR1_A, PCR2_A);
        verifier.revokePCRSet(PCR0_A, PCR1_A, PCR2_A);
        assertFalse(verifier.isPCRSetApproved(PCR0_A, PCR1_A, PCR2_A));

        // Re-approve A — the transition is real (was false), so the event MUST fire again.
        vm.prank(owner);
        vm.expectEmit(false, false, false, true);
        emit NitroAttestationVerifier.PCRSetApproved(PCR0_A, PCR1_A, PCR2_A);
        verifier.approvePCRSet(PCR0_A, PCR1_A, PCR2_A);
        assertTrue(verifier.isPCRSetApproved(PCR0_A, PCR1_A, PCR2_A));
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
                              CONSTANTS
    //////////////////////////////////////////////////////////////*/

    function test_MaxAge_Is60Minutes() public view {
        assertEq(verifier.MAX_AGE(), 60 minutes);
    }

    function test_ClockSkewTolerance_Is5Minutes() public view {
        assertEq(verifier.CLOCK_SKEW_TOLERANCE(), 5 minutes);
    }

    function test_MaxCabundleLen_Is8() public view {
        assertEq(verifier.MAX_CABUNDLE_LEN(), 8);
    }

    /*//////////////////////////////////////////////////////////////
                              FRESHNESS
    //////////////////////////////////////////////////////////////*/

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

    function test_Freshness_AcceptsExactlyMaxAge() public {
        // Boundary: `block.timestamp - docSeconds == MAX_AGE` MUST be
        // accepted; the predicate is strict `>`.
        vm.warp(1_000_000);
        uint64 boundaryMs = uint64((1_000_000 - 3600) * 1000);
        harness.checkFreshness(boundaryMs);
    }

    function test_Freshness_RejectsOneSecondPastMaxAge() public {
        vm.warp(1_000_000);
        uint64 ms_ = uint64((1_000_000 - 3601) * 1000);
        vm.expectRevert(abi.encodeWithSelector(NitroAttestationVerifier.AttestationStale.selector, 3601));
        harness.checkFreshness(ms_);
    }

    function test_Freshness_AcceptsSlightlyFuture() public {
        vm.warp(1_000_000);
        uint64 futureMs = uint64((1_000_000 + 4 * 60) * 1000);
        harness.checkFreshness(futureMs);
    }

    function test_Freshness_AcceptsExactlyClockSkewTolerance() public {
        // Boundary: `drift == CLOCK_SKEW_TOLERANCE` MUST be accepted.
        vm.warp(1_000_000);
        uint64 boundaryMs = uint64((1_000_000 + 300) * 1000);
        harness.checkFreshness(boundaryMs);
    }

    function test_Freshness_RejectsFarFuture() public {
        vm.warp(1_000_000);
        uint64 futureMs = uint64((1_000_000 + 6 * 60) * 1000);
        vm.expectRevert(abi.encodeWithSelector(NitroAttestationVerifier.AttestationFromFuture.selector, 360));
        harness.checkFreshness(futureMs);
    }

    function test_Freshness_RejectsOneSecondPastClockSkew() public {
        vm.warp(1_000_000);
        uint64 ms_ = uint64((1_000_000 + 301) * 1000);
        vm.expectRevert(abi.encodeWithSelector(NitroAttestationVerifier.AttestationFromFuture.selector, 301));
        harness.checkFreshness(ms_);
    }

    function test_Freshness_SubSecondTimestampRoundsDown() public {
        // 999ms past block.timestamp is treated as 0s drift (rounded down).
        // 1001ms past is 1s drift, still under MAX_AGE.
        vm.warp(1_000_000);
        // docSeconds = 1_000_000 + 999/1000 = 1_000_000 -> drift 0
        harness.checkFreshness(uint64(1_000_000 * 1000 + 999));
        // docSeconds = (1_000_000 - 1)*1000 + 999 = age 0 + 0.001 → 0s.
        harness.checkFreshness(uint64((1_000_000 - 1) * 1000 + 999));
    }

    function test_Freshness_TimestampZeroIsStale() public {
        // A zero timestamp (impossible per NSM but defensive) is rejected
        // as stale once block.timestamp > MAX_AGE.
        vm.warp(1_000_000);
        vm.expectRevert(abi.encodeWithSelector(NitroAttestationVerifier.AttestationStale.selector, 1_000_000));
        harness.checkFreshness(0);
    }

    /*//////////////////////////////////////////////////////////////
                    POST-VALIDATION BRANCH COVERAGE
    //////////////////////////////////////////////////////////////*/

    /// @dev Build a `Ptrs` struct from raw fields. Most fields are unused by
    ///      `_verifyValidated` and are left at their default (null/0) values.
    function _buildPtrs(uint64 timestamp, uint256 cabundleLen, CborElement[] memory pcrs, CborElement publicKey)
        internal
        pure
        returns (NitroValidator.Ptrs memory ptrs)
    {
        ptrs.timestamp = timestamp;
        ptrs.pcrs = pcrs;
        ptrs.cabundle = new CborElement[](cabundleLen);
        ptrs.publicKey = publicKey;
    }

    /// @dev Build a canonical PCR-element array of length `n` with each PCR
    ///      pointing at a 48-byte slot in the synthesised `attestationTbs`.
    function _pcrElems(uint256 n) internal pure returns (CborElement[] memory out) {
        out = new CborElement[](n);
        for (uint256 i = 0; i < n; i++) {
            // type 0x40 = byte string, start = i*48, length = 48
            out[i] = LibCborElement.toCborElement(0x40, i * 48, 48);
        }
    }

    /// @dev Build a synthetic attestationTbs containing 3 known PCR payloads
    ///      followed by a known 65-byte SEC1-uncompressed public key.
    function _buildTbsWithPcrsAndKey() internal pure returns (bytes memory tbs, bytes memory expectedKey) {
        // 3 * 48 bytes of PCRs + 65 bytes of key.
        tbs = new bytes(3 * 48 + 65);
        // Fill PCR0..2 with distinct sentinel bytes.
        for (uint256 i = 0; i < 48; i++) {
            tbs[0 * 48 + i] = bytes1(uint8(0xA0 + (i % 0x10)));
            tbs[1 * 48 + i] = bytes1(uint8(0xB0 + (i % 0x10)));
            tbs[2 * 48 + i] = bytes1(uint8(0xC0 + (i % 0x10)));
        }
        // Public key at offset 144: 0x04 || X (32) || Y (32)
        tbs[144] = 0x04;
        for (uint256 i = 1; i < 65; i++) {
            tbs[144 + i] = bytes1(uint8(0xD0 + (i % 0x10)));
        }
        expectedKey = new bytes(65);
        for (uint256 i = 0; i < 65; i++) {
            expectedKey[i] = tbs[144 + i];
        }
    }

    function _approvedPcrHashes() internal pure returns (bytes32 h0, bytes32 h1, bytes32 h2) {
        bytes memory p0 = new bytes(48);
        bytes memory p1 = new bytes(48);
        bytes memory p2 = new bytes(48);
        for (uint256 i = 0; i < 48; i++) {
            p0[i] = bytes1(uint8(0xA0 + (i % 0x10)));
            p1[i] = bytes1(uint8(0xB0 + (i % 0x10)));
            p2[i] = bytes1(uint8(0xC0 + (i % 0x10)));
        }
        h0 = keccak256(p0);
        h1 = keccak256(p1);
        h2 = keccak256(p2);
    }

    function test_VerifyValidated_HappyPath() public {
        (bytes memory tbs, bytes memory expectedKey) = _buildTbsWithPcrsAndKey();
        (bytes32 h0, bytes32 h1, bytes32 h2) = _approvedPcrHashes();
        vm.prank(owner);
        harness.approvePCRSet(h0, h1, h2);

        vm.warp(1_000_000);
        NitroValidator.Ptrs memory ptrs = _buildPtrs({
            timestamp: uint64(1_000_000 * 1000),
            cabundleLen: 3,
            pcrs: _pcrElems(3),
            publicKey: LibCborElement.toCborElement(0x40, 144, 65)
        });

        vm.expectEmit(true, false, false, true);
        emit NitroAttestationVerifier.AttestationVerified(keccak256(tbs), expectedKey, h0, h1, h2, ptrs.timestamp);
        (bytes memory pk, bytes32 pcr0, bytes32 pcr1, bytes32 pcr2) = harness.exposeVerifyValidated(tbs, ptrs);
        assertEq(pk, expectedKey);
        assertEq(pcr0, h0);
        assertEq(pcr1, h1);
        assertEq(pcr2, h2);
    }

    function test_VerifyValidated_RevertsOnCabundleTooLong() public {
        (bytes memory tbs,) = _buildTbsWithPcrsAndKey();
        NitroValidator.Ptrs memory ptrs = _buildPtrs({
            timestamp: uint64(block.timestamp * 1000),
            cabundleLen: 9,
            pcrs: _pcrElems(3),
            publicKey: LibCborElement.toCborElement(0x40, 144, 65)
        });
        vm.expectRevert(abi.encodeWithSelector(NitroAttestationVerifier.CabundleTooLong.selector, 9));
        harness.exposeVerifyValidated(tbs, ptrs);
    }

    function test_VerifyValidated_AcceptsCabundleAtBoundary() public {
        // cabundle.length == MAX_CABUNDLE_LEN (8) MUST be accepted.
        (bytes memory tbs,) = _buildTbsWithPcrsAndKey();
        (bytes32 h0, bytes32 h1, bytes32 h2) = _approvedPcrHashes();
        vm.prank(owner);
        harness.approvePCRSet(h0, h1, h2);
        vm.warp(1_000_000);

        NitroValidator.Ptrs memory ptrs = _buildPtrs({
            timestamp: uint64(1_000_000 * 1000),
            cabundleLen: 8,
            pcrs: _pcrElems(3),
            publicKey: LibCborElement.toCborElement(0x40, 144, 65)
        });
        harness.exposeVerifyValidated(tbs, ptrs);
    }

    function test_VerifyValidated_RevertsOnInsufficientPcrs() public {
        (bytes memory tbs,) = _buildTbsWithPcrsAndKey();
        NitroValidator.Ptrs memory ptrs = _buildPtrs({
            timestamp: uint64(block.timestamp * 1000),
            cabundleLen: 3,
            pcrs: _pcrElems(2),
            publicKey: LibCborElement.toCborElement(0x40, 144, 65)
        });
        vm.expectRevert(abi.encodeWithSelector(NitroAttestationVerifier.InsufficientPcrs.selector, 2));
        harness.exposeVerifyValidated(tbs, ptrs);
    }

    function test_VerifyValidated_RevertsOnInsufficientPcrsZero() public {
        (bytes memory tbs,) = _buildTbsWithPcrsAndKey();
        NitroValidator.Ptrs memory ptrs = _buildPtrs({
            timestamp: uint64(block.timestamp * 1000),
            cabundleLen: 3,
            pcrs: _pcrElems(0),
            publicKey: LibCborElement.toCborElement(0x40, 144, 65)
        });
        vm.expectRevert(abi.encodeWithSelector(NitroAttestationVerifier.InsufficientPcrs.selector, 0));
        harness.exposeVerifyValidated(tbs, ptrs);
    }

    function test_VerifyValidated_AcceptsExtraPcrs() public {
        // The contract only requires `pcrs.length >= 3` — extra PCRs must
        // not affect behaviour (AWS Nitro returns 16; we ignore PCR3+).
        (bytes memory tbs,) = _buildTbsWithPcrsAndKey();
        (bytes32 h0, bytes32 h1, bytes32 h2) = _approvedPcrHashes();
        vm.prank(owner);
        harness.approvePCRSet(h0, h1, h2);
        vm.warp(1_000_000);

        NitroValidator.Ptrs memory ptrs = _buildPtrs({
            timestamp: uint64(1_000_000 * 1000),
            cabundleLen: 3,
            pcrs: _pcrElems(16),
            publicKey: LibCborElement.toCborElement(0x40, 144, 65)
        });
        harness.exposeVerifyValidated(tbs, ptrs);
    }

    function test_VerifyValidated_RevertsWhenPCRSetNotApproved() public {
        // PCR set A is approved in setUp, but the synthetic PCRs hash to a
        // different triple → revert.
        (bytes memory tbs,) = _buildTbsWithPcrsAndKey();
        (bytes32 h0, bytes32 h1, bytes32 h2) = _approvedPcrHashes();
        // Do NOT approve {h0,h1,h2}; only PCR set A is approved in setUp.
        vm.warp(1_000_000);
        NitroValidator.Ptrs memory ptrs = _buildPtrs({
            timestamp: uint64(1_000_000 * 1000),
            cabundleLen: 3,
            pcrs: _pcrElems(3),
            publicKey: LibCborElement.toCborElement(0x40, 144, 65)
        });
        vm.expectRevert(abi.encodeWithSelector(NitroAttestationVerifier.PCRSetNotApproved.selector, h0, h1, h2));
        harness.exposeVerifyValidated(tbs, ptrs);
    }

    function test_VerifyValidated_RevertsWhenPCRSetWasRevoked() public {
        // After revoke, previously-approved PCRs MUST be rejected.
        (bytes memory tbs,) = _buildTbsWithPcrsAndKey();
        (bytes32 h0, bytes32 h1, bytes32 h2) = _approvedPcrHashes();
        vm.startPrank(owner);
        harness.approvePCRSet(h0, h1, h2);
        harness.revokePCRSet(h0, h1, h2);
        vm.stopPrank();
        vm.warp(1_000_000);

        NitroValidator.Ptrs memory ptrs = _buildPtrs({
            timestamp: uint64(1_000_000 * 1000),
            cabundleLen: 3,
            pcrs: _pcrElems(3),
            publicKey: LibCborElement.toCborElement(0x40, 144, 65)
        });
        vm.expectRevert(abi.encodeWithSelector(NitroAttestationVerifier.PCRSetNotApproved.selector, h0, h1, h2));
        harness.exposeVerifyValidated(tbs, ptrs);
    }

    function test_VerifyValidated_RevertsOnNullPublicKey() public {
        (bytes memory tbs,) = _buildTbsWithPcrsAndKey();
        (bytes32 h0, bytes32 h1, bytes32 h2) = _approvedPcrHashes();
        vm.prank(owner);
        harness.approvePCRSet(h0, h1, h2);
        vm.warp(1_000_000);

        // type 0xf6 == CBOR null
        CborElement nullKey = LibCborElement.toCborElement(0xf6, 0, 0);
        NitroValidator.Ptrs memory ptrs =
            _buildPtrs({timestamp: uint64(1_000_000 * 1000), cabundleLen: 3, pcrs: _pcrElems(3), publicKey: nullKey});
        vm.expectRevert(NitroAttestationVerifier.InvalidEmbeddedPublicKey.selector);
        harness.exposeVerifyValidated(tbs, ptrs);
    }

    function test_VerifyValidated_RevertsOnWrongKeyLength() public {
        // 64-byte key (just X || Y, missing 0x04 prefix), or 33-byte compressed,
        // etc. — any length other than 65 must be rejected.
        (bytes memory tbs,) = _buildTbsWithPcrsAndKey();
        (bytes32 h0, bytes32 h1, bytes32 h2) = _approvedPcrHashes();
        vm.prank(owner);
        harness.approvePCRSet(h0, h1, h2);
        vm.warp(1_000_000);

        NitroValidator.Ptrs memory ptrs = _buildPtrs({
            timestamp: uint64(1_000_000 * 1000),
            cabundleLen: 3,
            pcrs: _pcrElems(3),
            // 64-byte key starting at offset 144 — wrong length.
            publicKey: LibCborElement.toCborElement(0x40, 144, 64)
        });
        vm.expectRevert(NitroAttestationVerifier.InvalidEmbeddedPublicKey.selector);
        harness.exposeVerifyValidated(tbs, ptrs);
    }

    function test_VerifyValidated_RevertsOnCompressedKeyPrefix() public {
        // 65-byte length is OK, but the first byte is 0x02 (compressed-y-even
        // prefix). The contract is strict about uncompressed SEC1.
        (bytes memory tbs, bytes memory expectedKey) = _buildTbsWithPcrsAndKey();
        // Overwrite prefix byte.
        tbs[144] = 0x02;
        expectedKey; // silence unused-var warning

        (bytes32 h0, bytes32 h1, bytes32 h2) = _approvedPcrHashes();
        vm.prank(owner);
        harness.approvePCRSet(h0, h1, h2);
        vm.warp(1_000_000);

        NitroValidator.Ptrs memory ptrs = _buildPtrs({
            timestamp: uint64(1_000_000 * 1000),
            cabundleLen: 3,
            pcrs: _pcrElems(3),
            publicKey: LibCborElement.toCborElement(0x40, 144, 65)
        });
        vm.expectRevert(NitroAttestationVerifier.InvalidEmbeddedPublicKey.selector);
        harness.exposeVerifyValidated(tbs, ptrs);
    }

    function test_VerifyValidated_RevertsOnZeroLengthKey() public {
        (bytes memory tbs,) = _buildTbsWithPcrsAndKey();
        (bytes32 h0, bytes32 h1, bytes32 h2) = _approvedPcrHashes();
        vm.prank(owner);
        harness.approvePCRSet(h0, h1, h2);
        vm.warp(1_000_000);

        NitroValidator.Ptrs memory ptrs = _buildPtrs({
            timestamp: uint64(1_000_000 * 1000),
            cabundleLen: 3,
            pcrs: _pcrElems(3),
            publicKey: LibCborElement.toCborElement(0x40, 144, 0)
        });
        vm.expectRevert(NitroAttestationVerifier.InvalidEmbeddedPublicKey.selector);
        harness.exposeVerifyValidated(tbs, ptrs);
    }

    function test_VerifyValidated_FreshnessBranchTaken() public {
        // Stale timestamp must bubble up the freshness revert.
        (bytes memory tbs,) = _buildTbsWithPcrsAndKey();
        (bytes32 h0, bytes32 h1, bytes32 h2) = _approvedPcrHashes();
        vm.prank(owner);
        harness.approvePCRSet(h0, h1, h2);

        vm.warp(1_000_000);
        NitroValidator.Ptrs memory ptrs = _buildPtrs({
            timestamp: uint64((1_000_000 - 3601) * 1000),
            cabundleLen: 3,
            pcrs: _pcrElems(3),
            publicKey: LibCborElement.toCborElement(0x40, 144, 65)
        });
        vm.expectRevert(abi.encodeWithSelector(NitroAttestationVerifier.AttestationStale.selector, 3601));
        harness.exposeVerifyValidated(tbs, ptrs);
    }

    /*//////////////////////////////////////////////////////////////
                  REAL ATTESTATION FIXTURE END-TO-END
    //////////////////////////////////////////////////////////////*/

    /// @dev A real AWS-signed Nitro attestation document — the same fixture
    ///      used in `base/nitro-validator`'s {NitroValidator.t.sol}. We use
    ///      it to exercise the full {verifyAttestation} pipeline on real
    ///      bytes (CBOR parse → cert chain → P-384 → PCR allowlist → key).
    function _realAttestationTbs() internal pure returns (bytes memory) {
        return hex"846a5369676e61747572653144a101382240591144a9696d6f64756c655f69647827692d30646533386232623638353363633965382d656e633031393336383565376665653764383566646967657374665348413338346974696d657374616d701b000001937de1c5436470637273b0005830ec74bfbe7f7445a6c7610e152935e028276f638042b74797b119648e13f7a3675796b721034c320f140ea001b41aeae2015830fa2593b59f3e4fc7daba5cbdddfd3449d67cd02d43bb1128885e8f38b914d081dccdb68fff6d5b7a76bcb866a18a74a302583056ba201a72e36cd051e95e5c4724c899039b711770f4d9d4fe7a1de007119a10b364badcd35e90f728a5bdc9109057230358303c9cadd84f0d027d6a5370c3de4af9179824fd6f3f02ebab723ee4439c75d8f5183e1c55f523415d44e9e6580b06655204583098bdf1bde262272618ccd73279e8ee00dd2c36974bd253de55413a25ceb2cd7221421207c2c09dde609f87481b6f6c940558300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000658300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000758300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000858300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000958300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000a58300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000b58300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000c58300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000d58300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000e58300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000f58300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000006b63657274696669636174655902803082027c30820201a00302010202100193685e7fee7d8500000000674b3bd8300a06082a8648ce3d04030330818e310b30090603550406130255533113301106035504080c0a57617368696e67746f6e3110300e06035504070c0753656174746c65310f300d060355040a0c06416d617a6f6e310c300a060355040b0c034157533139303706035504030c30692d30646533386232623638353363633965382e75732d656173742d312e6177732e6e6974726f2d656e636c61766573301e170d3234313133303136323234355a170d3234313133303139323234385a308193310b30090603550406130255533113301106035504080c0a57617368696e67746f6e3110300e06035504070c0753656174746c65310f300d060355040a0c06416d617a6f6e310c300a060355040b0c03415753313e303c06035504030c35692d30646533386232623638353363633965382d656e63303139333638356537666565376438352e75732d656173742d312e6177733076301006072a8648ce3d020106052b810400220362000461d930c61be969237398264901d6a37282cfd42c0694d012d9143cc86a339d567913dae552bad2f10d47c50d4e670247f0344983cbdc2d2e0045d4ccbdff59ef7a26ebf1be83a81e24a651c92008fe9f465757792a0877fba02c8b5e1eb2ed90a31d301b300c0603551d130101ff04023000300b0603551d0f0404030206c0300a06082a8648ce3d0403030369003066023100e48f39a39b444a6e5ea7a38b808198a2318dd531ed62faf4a9223f71f27dff4a5e495e32dd10f250bbaf1f892a4d328f023100d09fc8e48e233b9e972eecb94798865664dbeb0d75b29041f482777a4b7cae133483dcc9d35509c4967be51db37a745468636162756e646c65845902153082021130820196a003020102021100f93175681b90afe11d46ccb4e4e7f856300a06082a8648ce3d0403033049310b3009060355040613025553310f300d060355040a0c06416d617a6f6e310c300a060355040b0c03415753311b301906035504030c126177732e6e6974726f2d656e636c61766573301e170d3139313032383133323830355a170d3439313032383134323830355a3049310b3009060355040613025553310f300d060355040a0c06416d617a6f6e310c300a060355040b0c03415753311b301906035504030c126177732e6e6974726f2d656e636c617665733076301006072a8648ce3d020106052b8104002203620004fc0254eba608c1f36870e29ada90be46383292736e894bfff672d989444b5051e534a4b1f6dbe3c0bc581a32b7b176070ede12d69a3fea211b66e752cf7dd1dd095f6f1370f4170843d9dc100121e4cf63012809664487c9796284304dc53ff4a3423040300f0603551d130101ff040530030101ff301d0603551d0e041604149025b50dd90547e796c396fa729dcf99a9df4b96300e0603551d0f0101ff040403020186300a06082a8648ce3d0403030369003066023100a37f2f91a1c9bd5ee7b8627c1698d255038e1f0343f95b63a9628c3d39809545a11ebcbf2e3b55d8aeee71b4c3d6adf3023100a2f39b1605b27028a5dd4ba069b5016e65b4fbde8fe0061d6a53197f9cdaf5d943bc61fc2beb03cb6fee8d2302f3dff65902c2308202be30820244a003020102021056bfc987fd05ac99c475061b1a65eedc300a06082a8648ce3d0403033049310b3009060355040613025553310f300d060355040a0c06416d617a6f6e310c300a060355040b0c03415753311b301906035504030c126177732e6e6974726f2d656e636c61766573301e170d3234313132383036303734355a170d3234313231383037303734355a3064310b3009060355040613025553310f300d060355040a0c06416d617a6f6e310c300a060355040b0c034157533136303406035504030c2d636264383238303866646138623434642e75732d656173742d312e6177732e6e6974726f2d656e636c617665733076301006072a8648ce3d020106052b81040022036200040713751f4391a24bf27d688c9fdde4b7eec0c4922af63f242186269602eca12354e79356170287baa07dd84fa89834726891f9b4b27032b3e86000d32471a79fbf1a30c1982ad4ed069ad96a7e11d9ae2b5cd6a93ad613ee559ed7f6385a9a89a381d53081d230120603551d130101ff040830060101ff020102301f0603551d230418301680149025b50dd90547e796c396fa729dcf99a9df4b96301d0603551d0e04160414bfbd54a168f57f7391b66ca60a2836f30acfb9a1300e0603551d0f0101ff040403020186306c0603551d1f046530633061a05fa05d865b687474703a2f2f6177732d6e6974726f2d656e636c617665732d63726c2e73332e616d617a6f6e6177732e636f6d2f63726c2f61623439363063632d376436332d343262642d396539662d3539333338636236376638342e63726c300a06082a8648ce3d0403030368003065023100c05dfd13378b1eecd926b0c3ba8da01eec89ec5502ae7ca73cb958557ca323057962fff2681993a0ab223b6eacf11033023035664252d7f9e2c89c988cc4164d390f898a5e8ac2e99dc58595aa4c624e93face7964026a99b4bcca7088b51250ccc459031a308203163082029ba003020102021100cb286a4a4a09207f8b0c14950dcd6861300a06082a8648ce3d0403033064310b3009060355040613025553310f300d060355040a0c06416d617a6f6e310c300a060355040b0c034157533136303406035504030c2d636264383238303866646138623434642e75732d656173742d312e6177732e6e6974726f2d656e636c61766573301e170d3234313133303033313435345a170d3234313230363031313435345a308189313c303a06035504030c33343762313739376131663031386266302e7a6f6e616c2e75732d656173742d312e6177732e6e6974726f2d656e636c61766573310c300a060355040b0c03415753310f300d060355040a0c06416d617a6f6e310b3009060355040613025553310b300906035504080c0257413110300e06035504070c0753656174746c653076301006072a8648ce3d020106052b810400220362000423959f700ef87dcbdba686449d944f2a89ad22aa03d73cf93d28853f2fb6a80b0cc714d3090e34cda8234eef8f804e46c0dcb216062afba3e2b36a693660d9965e2370308b8e1ffad8542ddbe3e733077481b0cbc747d8c7beb7612820d4fe95a381ea3081e730120603551d130101ff040830060101ff020101301f0603551d23041830168014bfbd54a168f57f7391b66ca60a2836f30acfb9a1301d0603551d0e04160414bbf52a3a42fdc4f301f72536b90e65aaa1b70a99300e0603551d0f0101ff0404030201863081800603551d1f047930773075a073a071866f687474703a2f2f63726c2d75732d656173742d312d6177732d6e6974726f2d656e636c617665732e73332e75732d656173742d312e616d617a6f6e6177732e636f6d2f63726c2f30366434386638652d326330382d343738312d613634352d6231646534303261656662382e63726c300a06082a8648ce3d0403030369003066023100fa31509230632a002939201eb5686b52d79f0276db5c2b954bed324caa5c3271a60d25e2e05a5e6700e488a074af4ecd02310084770462c2ef86dcdb11fa8a31dcf770866cbd28822b682a112b98c09a30e35e94affd3482bf8b01b59a0a7775b4af185902c3308202bf30820245a003020102021500c8925d382506d820d93d2c704a7523c4ba2ddfaa300a06082a8648ce3d040303308189313c303a06035504030c33343762313739376131663031386266302e7a6f6e616c2e75732d656173742d312e6177732e6e6974726f2d656e636c61766573310c300a060355040b0c03415753310f300d060355040a0c06416d617a6f6e310b3009060355040613025553310b300906035504080c0257413110300e06035504070c0753656174746c65301e170d3234313133303132343133315a170d3234313230313132343133315a30818e310b30090603550406130255533113301106035504080c0a57617368696e67746f6e3110300e06035504070c0753656174746c65310f300d060355040a0c06416d617a6f6e310c300a060355040b0c034157533139303706035504030c30692d30646533386232623638353363633965382e75732d656173742d312e6177732e6e6974726f2d656e636c617665733076301006072a8648ce3d020106052b8104002203620004466754b5718024df3564bcd722361e7c65a4922eda7b1f826758e30afac40b04a281062897d085311fd509b70a6bbc5f8280f86ae2ff255ad147146fc97b7afb16064f0712d335c1d473b716be320be625e91c5870973084b3a0005bc020c7b2a366306430120603551d130101ff040830060101ff020100300e0603551d0f0101ff040403020204301d0603551d0e04160414345c86a9ec55bc30cafd923d6b73111d9c57abc0301f0603551d23041830168014bbf52a3a42fdc4f301f72536b90e65aaa1b70a99300a06082a8648ce3d0403030368003065023100aba82c02f40acb9846012bf070578217eeb2ebbfd16414948438cf67eeab6f64cdc5a152998766c88b2cdebd5a97ebd402307421611ed511567bc8e6a0a2805b981ef38dc3bd6a6c661522802b5c5d658cc4fcc9b5e8df148b161d366926896736836a7075626c69635f6b657958410433a4701fa871b188983d570e2c2d8cf98fd66eb19ba8ca7617bc8e20e152a5d7f0205eae76e608ce855077e4565be69db4471ef72857253742f9602c11ff04e569757365725f64617461f6656e6f6e6365f6";
    }

    function _realAttestationSig() internal pure returns (bytes memory) {
        return hex"874e67088943e85654beb78443c747def2c3736bf93e2b52d033b3e936a04ead91f7b5a1229a1615f237f138f64399418b8046b6e40cd93e750b58f5e1aded45ebf3f103b9ea19a9b874142b576638dad2da142254ae913664649be22e0b83f9";
    }

    // The 48-byte raw PCR0/1/2 from the real fixture above.
    function _realPcr0Hash() internal pure returns (bytes32) {
        return keccak256(
            hex"ec74bfbe7f7445a6c7610e152935e028276f638042b74797b119648e13f7a3675796b721034c320f140ea001b41aeae2"
        );
    }

    function _realPcr1Hash() internal pure returns (bytes32) {
        return keccak256(
            hex"fa2593b59f3e4fc7daba5cbdddfd3449d67cd02d43bb1128885e8f38b914d081dccdb68fff6d5b7a76bcb866a18a74a3"
        );
    }

    function _realPcr2Hash() internal pure returns (bytes32) {
        return keccak256(
            hex"56ba201a72e36cd051e95e5c4724c899039b711770f4d9d4fe7a1de007119a10b364badcd35e90f728a5bdc910905723"
        );
    }

    // The 65-byte SEC1-uncompressed key embedded in the fixture.
    function _realPubKey() internal pure returns (bytes memory) {
        return hex"0433a4701fa871b188983d570e2c2d8cf98fd66eb19ba8ca7617bc8e20e152a5d7f0205eae76e608ce855077e4565be69db4471ef72857253742f9602c11ff04e5";
    }

    // The NSM-signed timestamp embedded in the fixture, in ms.
    uint64 internal constant REAL_DOC_TS_MS = 1_732_983_768_387;
    uint256 internal constant REAL_DOC_TS = uint256(REAL_DOC_TS_MS) / 1000;

    function _warpToRealAttestationWindow() internal {
        // Warp ~3 minutes after the doc timestamp — well within MAX_AGE and
        // within the leaf certificate's validity window.
        vm.warp(REAL_DOC_TS + 3 minutes);
    }

    function test_VerifyAttestation_RealFixture_HappyPath() public {
        // Pre-approve the real fixture's PCR set.
        vm.prank(owner);
        verifier.approvePCRSet(_realPcr0Hash(), _realPcr1Hash(), _realPcr2Hash());

        _warpToRealAttestationWindow();

        bytes memory tbs = _realAttestationTbs();
        bytes memory sig = _realAttestationSig();

        vm.expectEmit(true, false, false, true);
        emit NitroAttestationVerifier.AttestationVerified(
            keccak256(tbs), _realPubKey(), _realPcr0Hash(), _realPcr1Hash(), _realPcr2Hash(), REAL_DOC_TS_MS
        );
        (bytes memory pk, bytes32 p0, bytes32 p1, bytes32 p2) = verifier.verifyAttestation(tbs, sig);
        assertEq(pk, _realPubKey());
        assertEq(p0, _realPcr0Hash());
        assertEq(p1, _realPcr1Hash());
        assertEq(p2, _realPcr2Hash());
    }

    function test_VerifyAttestation_RealFixture_RevertsWhenPCRSetNotApproved() public {
        // Do NOT approve the fixture's PCRs (only A is approved in setUp).
        _warpToRealAttestationWindow();
        bytes memory tbs = _realAttestationTbs();
        bytes memory sig = _realAttestationSig();
        vm.expectRevert(
            abi.encodeWithSelector(
                NitroAttestationVerifier.PCRSetNotApproved.selector, _realPcr0Hash(), _realPcr1Hash(), _realPcr2Hash()
            )
        );
        verifier.verifyAttestation(tbs, sig);
    }

    function test_VerifyAttestation_RealFixture_RevertsWhenStale() public {
        vm.prank(owner);
        verifier.approvePCRSet(_realPcr0Hash(), _realPcr1Hash(), _realPcr2Hash());

        // Warp MAX_AGE + 1 second past doc timestamp → stale by 1 second.
        // The leaf cert is valid for ~3 hours, so it's still in window.
        uint256 ts = REAL_DOC_TS + verifier.MAX_AGE() + 1;
        vm.warp(ts);

        bytes memory tbs = _realAttestationTbs();
        bytes memory sig = _realAttestationSig();
        vm.expectRevert(abi.encodeWithSelector(NitroAttestationVerifier.AttestationStale.selector, ts - REAL_DOC_TS));
        verifier.verifyAttestation(tbs, sig);
    }

    function test_VerifyAttestation_RealFixture_RevertsWhenFromFuture_NotTestable() public pure {
        // The fixture's doc timestamp (1_732_983_768) is only ~3 seconds
        // after the leaf certificate's `notBefore` (1_732_983_765). To trip
        // {AttestationFromFuture} we'd need to warp to at least
        // CLOCK_SKEW_TOLERANCE (5 minutes) BEFORE the doc timestamp, at which
        // point the leaf cert isn't yet valid and {CertManager} reverts with
        // "certificate not valid yet" before {_checkFreshness} runs.
        //
        // The future-drift branch is covered exhaustively via the harness:
        // see {test_Freshness_RejectsFarFuture},
        // {test_Freshness_RejectsOneSecondPastClockSkew}, and
        // {test_VerifyValidated_FreshnessBranchTaken}.
    }
}
