// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test, Vm} from "forge-std/Test.sol";
import {NitroEnclaveKeyRegistry} from "../../../src/dispute/nitro/NitroEnclaveKeyRegistry.sol";
import {NitroProofVerifier} from "../../../src/dispute/nitro/NitroProofVerifier.sol";
import {LibProof, TransitionPublicValues} from "../../../src/dispute/lib/LibProof.sol";
import {MockNitroAttestationVerifier} from "./mocks/MockNitroAttestationVerifier.sol";

/// @title NitroEndToEndTest
/// @notice Full pipeline integration test wiring:
///         `NitroAttestationVerifier`-like attestation flow (via mock so we
///         can control the certified enclave key) → `NitroEnclaveKeyRegistry`
///         → `NitroProofVerifier`.
///
/// @dev A truly end-to-end test that goes through a real AWS-signed Nitro
///      attestation AND a real `NitroProofVerifier` verification would
///      require knowing the enclave's private key (so we could sign a fresh
///      `transition_commitment`). Since we obviously don't have AWS NSM's
///      private key, the integration test mocks the attestation step but
///      otherwise exercises the registry + proof-verifier code paths exactly
///      as they run in production. The PCR-allowlist piece of the
///      `NitroAttestationVerifier` contract is covered separately in
///      `NitroAttestationVerifierTest` (including a real-fixture happy
///      path).
contract NitroEndToEndTest is Test {
    MockNitroAttestationVerifier attestationVerifier;
    NitroEnclaveKeyRegistry registry;
    NitroProofVerifier proofVerifier;

    address owner = makeAddr("integration-owner");

    bytes32 constant PCR0 = bytes32(uint256(0xC0FFEE));
    bytes32 constant PCR1 = bytes32(uint256(0xBEEF));
    bytes32 constant PCR2 = bytes32(uint256(0xCAFE));

    bytes32 constant ROOT_ID = keccak256("root-id");
    bytes32 constant L1H = keccak256("l1-origin");
    bytes32 constant CFG = keccak256("rollup-cfg");
    bytes32 constant PRE = keccak256("pre-root");
    uint64 constant PRE_BLK = 41_999;
    bytes32 constant POST = keccak256("post-root");
    uint64 constant BLK = 42_000;

    bytes constant TBS = hex"abcdabcd";
    bytes constant SIG = hex"feedfeed";

    Vm.Wallet enclaveWallet;
    bytes enclavePubKey;

    function setUp() public {
        attestationVerifier = new MockNitroAttestationVerifier();
        registry = new NitroEnclaveKeyRegistry(attestationVerifier, owner);
        proofVerifier = new NitroProofVerifier(registry);

        enclaveWallet = vm.createWallet("enclave-integration");
        enclavePubKey = _uncompressedKey(enclaveWallet.publicKeyX, enclaveWallet.publicKeyY);
        attestationVerifier.setExpectation(TBS, SIG, enclavePubKey, PCR0, PCR1, PCR2);
    }

    function _uncompressedKey(uint256 x, uint256 y) internal pure returns (bytes memory out) {
        out = new bytes(65);
        out[0] = 0x04;
        assembly {
            mstore(add(out, 33), x)
            mstore(add(out, 65), y)
        }
    }

    function _transition(bytes32 postRoot, uint64 blk, bytes32 cfg)
        internal
        pure
        returns (TransitionPublicValues memory)
    {
        return TransitionPublicValues({
            l1Head: L1H,
            l2PreRoot: PRE,
            l2PreBlockNumber: PRE_BLK,
            l2PostRoot: postRoot,
            l2PostBlockNumber: blk,
            rollupConfigHash: cfg
        });
    }

    function _signCommitment(Vm.Wallet memory w, TransitionPublicValues memory transition)
        internal
        returns (bytes memory)
    {
        bytes32 commitment = keccak256(abi.encode(transition));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(w, commitment);
        return abi.encodePacked(r, s, v);
    }

    function _verify(TransitionPublicValues memory transition, bytes memory sig) internal view returns (bool) {
        return proofVerifier.verify(sig, PCR0, abi.encode(transition));
    }

    /*//////////////////////////////////////////////////////////////
                              FULL PIPELINE
    //////////////////////////////////////////////////////////////*/

    function test_E2E_RegisterThenVerify() public {
        // 1. (NitroAttestationVerifier owner would call) approvePCRSet —
        //    elided here because the mock does not enforce the allowlist.
        //    See NitroAttestationVerifierTest for the real-fixture coverage.
        //
        // 2. Anyone calls registry.registerKey(tbs, sig). The verifier
        //    surfaces the enclave key + PCRs; the registry stores its signer address.
        vm.expectEmit(true, false, false, true);
        emit NitroEnclaveKeyRegistry.SignerRegistered(enclaveWallet.addr, PCR0, PCR1, PCR2);
        registry.registerKey(TBS, SIG, "");
        assertTrue(registry.isSignerRegistered(enclaveWallet.addr));
        assertEq(uint8(registry.signerStatus(enclaveWallet.addr)), uint8(NitroEnclaveKeyRegistry.SignerStatus.Active));

        // 3. The (live) enclave signs a signing-commitment for the transition
        //    the game expects; the defender submits the signature as the proof.
        TransitionPublicValues memory transition = _transition(POST, BLK, CFG);
        assertTrue(_verify(transition, _signCommitment(enclaveWallet, transition)));
    }

    function test_E2E_RevokeSignerInvalidatesFutureProofs() public {
        registry.registerKey(TBS, SIG, "");
        TransitionPublicValues memory transition = _transition(POST, BLK, CFG);
        bytes memory sig = _signCommitment(enclaveWallet, transition);

        // Pre-revoke: proof is valid.
        assertTrue(_verify(transition, sig));

        // Owner revokes the key (e.g. on compromise).
        vm.prank(owner);
        registry.revokeSigner(enclaveWallet.addr);
        assertTrue(registry.isSignerRevoked(enclaveWallet.addr));

        // Same (previously valid) proof MUST now be rejected — the proof
        // verifier consults the registry on every call.
        assertFalse(_verify(transition, sig));

        // And the registry must permanently refuse to re-register the key,
        // even via a fresh attestation.
        vm.expectRevert(NitroEnclaveKeyRegistry.SignerRevokedPermanently.selector);
        registry.registerKey(TBS, SIG, "");
    }

    function test_E2E_TwoEnclavesSameImageBothValid() public {
        // Image A produces two distinct enclave instances with two distinct
        // ephemeral keys but the same PCR triple. Both register, both sign
        // independently, both verify.
        Vm.Wallet memory secondWallet = vm.createWallet("enclave-integration-2");
        bytes memory secondPubKey = _uncompressedKey(secondWallet.publicKeyX, secondWallet.publicKeyY);
        bytes memory tbs2 = hex"caca";
        attestationVerifier.setExpectation(tbs2, SIG, secondPubKey, PCR0, PCR1, PCR2);

        registry.registerKey(TBS, SIG, "");
        registry.registerKey(tbs2, SIG, "");

        TransitionPublicValues memory transition = _transition(POST, BLK, CFG);
        assertTrue(_verify(transition, _signCommitment(enclaveWallet, transition)));
        assertTrue(_verify(transition, _signCommitment(secondWallet, transition)));
    }

    function test_E2E_RevokeOneEnclaveDoesNotAffectPeer() public {
        // Same multi-enclave setup, but revoking enclave A's key must not
        // invalidate enclave B's still-running key.
        Vm.Wallet memory secondWallet = vm.createWallet("enclave-integration-3");
        bytes memory secondPubKey = _uncompressedKey(secondWallet.publicKeyX, secondWallet.publicKeyY);
        bytes memory tbs2 = hex"baba";
        attestationVerifier.setExpectation(tbs2, SIG, secondPubKey, PCR0, PCR1, PCR2);

        registry.registerKey(TBS, SIG, "");
        registry.registerKey(tbs2, SIG, "");

        vm.prank(owner);
        registry.revokeSigner(enclaveWallet.addr);

        TransitionPublicValues memory transition = _transition(POST, BLK, CFG);
        assertFalse(_verify(transition, _signCommitment(enclaveWallet, transition)));
        assertTrue(_verify(transition, _signCommitment(secondWallet, transition)));
    }

    function test_E2E_UnregisteredSignerFails() public {
        // Skip registration; the proof verifier MUST refuse even a
        // cryptographically-valid signature from an unknown key.
        TransitionPublicValues memory transition = _transition(POST, BLK, CFG);
        assertFalse(_verify(transition, _signCommitment(enclaveWallet, transition)));
    }

    function test_E2E_ProofMustBindToExpectedTransition() public {
        registry.registerKey(TBS, SIG, "");

        // Honest signature over a different transition than the game expects.
        TransitionPublicValues memory proven = _transition(POST, BLK + 1, CFG);
        bytes memory sig = _signCommitment(enclaveWallet, proven);
        assertFalse(_verify(_transition(POST, BLK, CFG), sig));
    }
}
