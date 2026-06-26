// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {NitroAttestationVerifier} from "../../src/proofs/nitro/NitroAttestationVerifier.sol";
import {CertManager} from "../../src/proofs/nitro/vendor/nitro-validator/CertManager.sol";
import {ICertManager} from "../../src/proofs/nitro/vendor/nitro-validator/ICertManager.sol";

/// @dev Test harness exposing the internal freshness check so we can exercise
///      both the stale and the future-dated branches without constructing a
///      full Nitro attestation document.
contract NitroAttestationVerifierHarness is NitroAttestationVerifier {
    constructor(ICertManager cm) NitroAttestationVerifier(cm) {}
    function checkFreshness(uint64 timestampMs) external view { _checkFreshness(timestampMs); }
}

/// @dev Smoke tests for {NitroAttestationVerifier}. The full happy-path verification
///      of a real Nitro attestation document requires a fresh, signed AWS NSM
///      document and the full intermediate cert bundle, which is not practical to
///      check in via fixtures. The underlying parser/cert-chain/P-384 verification
///      logic is exhaustively tested upstream in Base's `nitro-validator` repo
///      (vendored under `src/proofs/nitro/vendor/nitro-validator`).
///
///      These tests confirm that:
///        - the contract deploys and wires {CertManager} correctly;
///        - constants like {MAX_AGE} are surfaced;
///        - the contract is a {NitroValidator} (i.e. the inheritance compiles).
///
///      End-to-end verification with a captured Nitro document should be added as a
///      fork test against a network with deployed fixtures.
contract NitroAttestationVerifierTest is Test {
    CertManager certManager;
    NitroAttestationVerifier verifier;
    NitroAttestationVerifierHarness harness;

    function setUp() public {
        certManager = new CertManager();
        verifier = new NitroAttestationVerifier(ICertManager(address(certManager)));
        harness = new NitroAttestationVerifierHarness(ICertManager(address(certManager)));
    }

    function test_Constructor_WiresCertManager() public view {
        assertEq(address(verifier.certManager()), address(certManager));
    }

    function test_MaxAge_Is60Minutes() public view {
        assertEq(verifier.MAX_AGE(), 60 minutes);
    }

    function test_VerifyAttestation_RevertsOnGarbageInput() public {
        // Anything that fails CBOR parsing or the cert chain should revert. We don't
        // assert a specific selector — there are many failure modes in the parser —
        // just that random bytes do not silently verify.
        vm.expectRevert();
        verifier.verifyAttestation(hex"00", hex"00", bytes32(0), bytes32(0), bytes32(0));
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
        // 4 minutes in the future is within CLOCK_SKEW_TOLERANCE (5 minutes).
        uint64 futureMs = uint64((1_000_000 + 4 * 60) * 1000);
        harness.checkFreshness(futureMs);
    }

    function test_Freshness_RejectsFarFuture() public {
        vm.warp(1_000_000);
        // 6 minutes in the future exceeds CLOCK_SKEW_TOLERANCE.
        uint64 futureMs = uint64((1_000_000 + 6 * 60) * 1000);
        vm.expectRevert(abi.encodeWithSelector(NitroAttestationVerifier.AttestationFromFuture.selector, 360));
        harness.checkFreshness(futureMs);
    }
}
