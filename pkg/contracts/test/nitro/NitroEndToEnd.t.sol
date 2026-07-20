// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test, Vm} from "forge-std/Test.sol";
import {NitroAttestationVerifier} from "../../src/proofs/nitro/NitroAttestationVerifier.sol";
import {NitroEnclaveKeyRegistry} from "../../src/proofs/nitro/NitroEnclaveKeyRegistry.sol";
import {NitroProofVerifier} from "../../src/proofs/nitro/NitroProofVerifier.sol";
import {INitroAttestationVerifier} from "../../src/proofs/nitro/INitroAttestationVerifier.sol";
import {WorldChainProofLib} from "../../src/proofs/WorldChainProofLib.sol";
import {MockProofSystemFactory, MockProofSystemGame} from "../mocks/MockProofSystemGame.sol";
import {MockNitroAttestationVerifier} from "./mocks/MockNitroAttestationVerifier.sol";

contract EndToEndParentGame {
    bytes32 public rootClaim;

    constructor(bytes32 rootClaim_) {
        rootClaim = rootClaim_;
    }
}

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

    bytes32 constant L1H = keccak256("l1-origin");
    uint256 constant L1N = 7_777;
    bytes32 constant CFG = keccak256("rollup-cfg");
    bytes32 constant PRE = keccak256("pre-root");
    uint64 constant PRE_BLK = 41_999;
    bytes32 constant POST = keccak256("post-root");
    uint64 constant BLK = 42_000;
    address constant ANCHOR_STATE_REGISTRY = address(0xA11CE);

    bytes constant TBS = hex"abcdabcd";
    bytes constant SIG = hex"feedfeed";

    Vm.Wallet enclaveWallet;
    bytes enclavePubKey;
    EndToEndParentGame parent;
    MockProofSystemGame game;
    MockProofSystemFactory proofSystemFactory;
    bytes32 domainHash;

    function setUp() public {
        attestationVerifier = new MockNitroAttestationVerifier();
        registry = new NitroEnclaveKeyRegistry(attestationVerifier, owner);
        parent = new EndToEndParentGame(PRE);
        proofVerifier = new NitroProofVerifier(registry);
        WorldChainProofLib.Domain memory domain = WorldChainProofLib.Domain({
            chainId: 480, proofSystemVersion: 1, rollupConfigHash: CFG, blockInterval: BLK - PRE_BLK
        });
        proofSystemFactory = new MockProofSystemFactory(domain);
        domainHash = WorldChainProofLib.domainHash(domain);
        game = new MockProofSystemGame();
        game.setContext(
            MockProofSystemGame.Context({
                factory: address(proofSystemFactory),
                rootId: _rootId(POST, BLK),
                anchorStateRegistry: ANCHOR_STATE_REGISTRY,
                domainHash: domainHash,
                parentRef: address(parent),
                startingRootClaim: PRE,
                startingL2BlockNumber: PRE_BLK,
                rootClaim: POST,
                l2BlockNumber: BLK,
                l1OriginHash: L1H,
                l1OriginNumber: L1N
            })
        );

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
        returns (WorldChainProofLib.TransitionPublicValues memory)
    {
        return WorldChainProofLib.TransitionPublicValues({
            l1Head: L1H,
            l2PreRoot: PRE,
            l2PreBlockNumber: PRE_BLK,
            l2PostRoot: postRoot,
            l2PostBlockNumber: blk,
            rollupConfigHash: cfg
        });
    }

    function _signCommitment(Vm.Wallet memory w, WorldChainProofLib.TransitionPublicValues memory transition)
        internal
        returns (bytes memory)
    {
        bytes32 commitment = keccak256(abi.encode(transition));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(w, commitment);
        return abi.encodePacked(r, s, v);
    }

    function _signCommitment(Vm.Wallet memory w, bytes32 postRoot, uint64 blk, bytes32 cfg)
        internal
        returns (bytes memory)
    {
        return _signCommitment(w, _transition(postRoot, blk, cfg));
    }

    function _proof(bytes memory sig, bytes memory pub, WorldChainProofLib.TransitionPublicValues memory transition)
        internal
        view
        returns (bytes memory)
    {
        return abi.encode(domainHash, address(parent), L1N, transition, sig, pub);
    }

    function _proof(bytes memory sig, bytes memory pub, bytes32 postRoot, uint64 blk, bytes32 cfg)
        internal
        view
        returns (bytes memory)
    {
        return _proof(sig, pub, _transition(postRoot, blk, cfg));
    }

    function _rootId(bytes32 postRoot, uint64 blk) internal view returns (bytes32) {
        return WorldChainProofLib.rootId(domainHash, address(parent), postRoot, uint256(blk), L1H, L1N);
    }

    function _verify(bytes32 rootId, bytes memory proof) internal view returns (bool) {
        return game.verify(address(proofVerifier), rootId, proof);
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
        //    surfaces the enclave key + PCRs; the registry stores them.
        vm.expectEmit(false, false, false, true);
        emit NitroEnclaveKeyRegistry.KeyRegistered(enclavePubKey, PCR0, PCR1, PCR2);
        registry.registerKey(TBS, SIG, "");
        assertTrue(registry.isKeyRegistered(enclavePubKey));
        assertEq(uint8(registry.keyStatus(enclavePubKey)), uint8(NitroEnclaveKeyRegistry.KeyStatus.Active));

        // 3. The (live) enclave signs a signing-commitment for some rollup
        //    boot-info. The proposer wraps it up into a NitroProofVerifier
        //    proof tuple and submits it to dispute-game resolution.
        bytes memory sig = _signCommitment(enclaveWallet, POST, BLK, CFG);
        assertTrue(_verify(_rootId(POST, BLK), _proof(sig, enclavePubKey, POST, BLK, CFG)));
    }

    function test_E2E_RevokeKeyInvalidatesFutureProofs() public {
        registry.registerKey(TBS, SIG, "");
        bytes memory sig = _signCommitment(enclaveWallet, POST, BLK, CFG);

        // Pre-revoke: proof is valid.
        assertTrue(_verify(_rootId(POST, BLK), _proof(sig, enclavePubKey, POST, BLK, CFG)));

        // Owner revokes the key (e.g. on compromise).
        vm.prank(owner);
        registry.revokeKey(enclavePubKey);
        assertTrue(registry.isKeyRevoked(enclavePubKey));

        // Same (previously valid) proof MUST now be rejected — the proof
        // verifier consults the registry on every call.
        assertFalse(_verify(_rootId(POST, BLK), _proof(sig, enclavePubKey, POST, BLK, CFG)));

        // And the registry must permanently refuse to re-register the key,
        // even via a fresh attestation.
        vm.expectRevert(NitroEnclaveKeyRegistry.KeyRevokedPermanently.selector);
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

        bytes memory sigA = _signCommitment(enclaveWallet, POST, BLK, CFG);
        bytes memory sigB = _signCommitment(secondWallet, POST, BLK, CFG);

        assertTrue(_verify(_rootId(POST, BLK), _proof(sigA, enclavePubKey, POST, BLK, CFG)));
        assertTrue(_verify(_rootId(POST, BLK), _proof(sigB, secondPubKey, POST, BLK, CFG)));
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
        registry.revokeKey(enclavePubKey);

        bytes memory sigA = _signCommitment(enclaveWallet, POST, BLK, CFG);
        bytes memory sigB = _signCommitment(secondWallet, POST, BLK, CFG);
        assertFalse(_verify(_rootId(POST, BLK), _proof(sigA, enclavePubKey, POST, BLK, CFG)));
        assertTrue(_verify(_rootId(POST, BLK), _proof(sigB, secondPubKey, POST, BLK, CFG)));
    }

    function test_E2E_UnregisteredKeyFails() public {
        // Skip registration; the proof verifier MUST refuse even a
        // cryptographically-valid signature from an unknown key.
        bytes memory sig = _signCommitment(enclaveWallet, POST, BLK, CFG);
        assertFalse(_verify(_rootId(POST, BLK), _proof(sig, enclavePubKey, POST, BLK, CFG)));
    }

    function test_E2E_ProofMustBindToRequestedRootId() public {
        registry.registerKey(TBS, SIG, "");
        bytes memory sig = _signCommitment(enclaveWallet, POST, BLK, CFG);

        // Honest proof but the dispute game asks about a different rootId.
        assertFalse(_verify(bytes32(uint256(0xdead)), _proof(sig, enclavePubKey, POST, BLK, CFG)));
    }
}
