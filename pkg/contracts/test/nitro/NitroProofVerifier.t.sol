// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test, Vm} from "forge-std/Test.sol";
import {NitroEnclaveKeyRegistry} from "../../src/proofs/nitro/NitroEnclaveKeyRegistry.sol";
import {NitroProofVerifier} from "../../src/proofs/nitro/NitroProofVerifier.sol";
import {WorldChainProofLib} from "../../src/proofs/WorldChainProofLib.sol";
import {MockProofSystemGame} from "../mocks/MockProofSystemGame.sol";
import {MockNitroAttestationVerifier} from "./mocks/MockNitroAttestationVerifier.sol";

contract MockParentGame {
    bytes32 public rootClaim;

    constructor(bytes32 rootClaim_) {
        rootClaim = rootClaim_;
    }
}

contract NitroProofVerifierTest is Test {
    MockNitroAttestationVerifier attestationVerifier;
    NitroEnclaveKeyRegistry registry;
    NitroProofVerifier proofVerifier;

    address owner = makeAddr("owner");

    bytes32 constant PCR0 = bytes32(uint256(0xa));
    bytes32 constant PCR1 = bytes32(uint256(0xb));
    bytes32 constant PCR2 = bytes32(uint256(0xc));

    bytes constant TBS = hex"deadbeef";
    bytes constant SIG = hex"cafebabe";

    // Transition public values used to build a signing commitment.
    bytes32 constant L2_PRE_ROOT = keccak256("l2-pre-root");
    uint64 constant L2_PRE_BLOCK = 123_455;
    bytes32 constant L2_POST_ROOT = keccak256("l2-post-root");
    uint64 constant L2_BLOCK = 123_456;
    bytes32 constant ROLLUP_CFG = keccak256("rollup-cfg");

    // Context fields needed to rebuild rootId.
    bytes32 constant DOMAIN_HASH = keccak256("domain");
    bytes32 constant L1_ORIGIN_HASH = keccak256("l1-origin");
    uint256 constant L1_ORIGIN_NUMBER = 9_001;
    address constant ANCHOR_STATE_REGISTRY = address(0xA11CE);

    Vm.Wallet enclaveWallet;
    bytes enclavePubKey;
    MockParentGame parent;
    MockProofSystemGame game;

    function setUp() public {
        attestationVerifier = new MockNitroAttestationVerifier();
        registry = new NitroEnclaveKeyRegistry(attestationVerifier, owner);
        parent = new MockParentGame(L2_PRE_ROOT);
        proofVerifier = new NitroProofVerifier(registry);
        game = new MockProofSystemGame();
        _setGameContext(_transition());

        enclaveWallet = vm.createWallet("enclave");
        enclavePubKey = _uncompressedKey(enclaveWallet.publicKeyX, enclaveWallet.publicKeyY);

        attestationVerifier.setExpectation(TBS, SIG, enclavePubKey, PCR0, PCR1, PCR2);
        registry.registerKey(TBS, SIG, "");
    }

    /*//////////////////////////////////////////////////////////////
                                HELPERS
    //////////////////////////////////////////////////////////////*/

    function _uncompressedKey(uint256 x, uint256 y) internal pure returns (bytes memory out) {
        out = new bytes(65);
        out[0] = 0x04;
        assembly {
            mstore(add(out, 33), x)
            mstore(add(out, 65), y)
        }
    }

    function _sign(Vm.Wallet memory w, bytes32 digest) internal returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(w, digest);
        return abi.encodePacked(r, s, v);
    }

    function _sign(bytes32 digest) internal returns (bytes memory) {
        return _sign(enclaveWallet, digest);
    }

    function _transition() internal pure returns (WorldChainProofLib.TransitionPublicValues memory) {
        return WorldChainProofLib.TransitionPublicValues({
            l1Head: L1_ORIGIN_HASH,
            l2PreRoot: L2_PRE_ROOT,
            l2PreBlockNumber: L2_PRE_BLOCK,
            l2PostRoot: L2_POST_ROOT,
            l2PostBlockNumber: L2_BLOCK,
            rollupConfigHash: ROLLUP_CFG
        });
    }

    function _commitment() internal pure returns (bytes32) {
        return keccak256(abi.encode(_transition()));
    }

    function _expectedRootId() internal view returns (bytes32) {
        return WorldChainProofLib.rootId(
            DOMAIN_HASH, address(parent), L2_POST_ROOT, uint256(L2_BLOCK), L1_ORIGIN_HASH, L1_ORIGIN_NUMBER
        );
    }

    function _proofBytes(bytes memory sig, bytes memory pub) internal view returns (bytes memory) {
        return abi.encode(DOMAIN_HASH, address(parent), L1_ORIGIN_NUMBER, _transition(), sig, pub);
    }

    function _setGameContext(WorldChainProofLib.TransitionPublicValues memory transition) internal {
        bytes32 rootId = WorldChainProofLib.rootId(
            DOMAIN_HASH,
            address(parent),
            transition.l2PostRoot,
            uint256(transition.l2PostBlockNumber),
            transition.l1Head,
            L1_ORIGIN_NUMBER
        );
        game.setContext(
            MockProofSystemGame.Context({
                rootId: rootId,
                anchorStateRegistry: ANCHOR_STATE_REGISTRY,
                domainHash: DOMAIN_HASH,
                parentRef: address(parent),
                startingRootClaim: L2_PRE_ROOT,
                startingL2BlockNumber: L2_PRE_BLOCK,
                rootClaim: transition.l2PostRoot,
                l2BlockNumber: transition.l2PostBlockNumber,
                l1OriginHash: transition.l1Head,
                l1OriginNumber: L1_ORIGIN_NUMBER
            })
        );
    }

    function _verify(bytes32 rootId, bytes memory proof) internal view returns (bool) {
        return game.verify(address(proofVerifier), rootId, proof);
    }

    /*//////////////////////////////////////////////////////////////
                              HAPPY PATH
    //////////////////////////////////////////////////////////////*/

    function test_Verify_HappyPath() public {
        bytes memory sig = _sign(_commitment());
        assertTrue(_verify(_expectedRootId(), _proofBytes(sig, enclavePubKey)));
    }

    /*//////////////////////////////////////////////////////////////
                            BINDING FAILURES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseForWrongRootId() public {
        // Honest signature + transition public values, but the game asks about a different
        // rootId — the verifier must NOT validate.
        bytes memory sig = _sign(_commitment());
        assertFalse(_verify(bytes32(uint256(0xdead)), _proofBytes(sig, enclavePubKey)));
    }

    function test_Verify_FalseForWrongBootInfo() public {
        WorldChainProofLib.TransitionPublicValues memory wrongTransition = _transition();
        wrongTransition.l2PostBlockNumber += 1;
        bytes memory sig = _sign(keccak256(abi.encode(wrongTransition)));
        bytes32 wrongRootId = WorldChainProofLib.rootId(
            DOMAIN_HASH,
            address(parent),
            L2_POST_ROOT,
            uint256(wrongTransition.l2PostBlockNumber),
            L1_ORIGIN_HASH,
            L1_ORIGIN_NUMBER
        );
        bytes memory proof =
            abi.encode(DOMAIN_HASH, address(parent), L1_ORIGIN_NUMBER, wrongTransition, sig, enclavePubKey);
        assertFalse(_verify(wrongRootId, proof));
    }

    function test_Verify_FalseForWrongPreRoot() public {
        WorldChainProofLib.TransitionPublicValues memory wrongTransition = _transition();
        wrongTransition.l2PreRoot = keccak256("wrong-pre-root");
        bytes memory sig = _sign(keccak256(abi.encode(wrongTransition)));
        bytes memory proof =
            abi.encode(DOMAIN_HASH, address(parent), L1_ORIGIN_NUMBER, wrongTransition, sig, enclavePubKey);
        assertFalse(_verify(_expectedRootId(), proof));
    }

    function test_Verify_FalseForWrongPreBlockNumber() public {
        WorldChainProofLib.TransitionPublicValues memory wrongTransition = _transition();
        wrongTransition.l2PreBlockNumber += 1;
        bytes memory sig = _sign(keccak256(abi.encode(wrongTransition)));
        bytes memory proof =
            abi.encode(DOMAIN_HASH, address(parent), L1_ORIGIN_NUMBER, wrongTransition, sig, enclavePubKey);

        assertFalse(_verify(_expectedRootId(), proof));
    }

    /*//////////////////////////////////////////////////////////////
                            REGISTRY GATES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseForUnregisteredKey() public {
        Vm.Wallet memory rogue = vm.createWallet("rogue");
        bytes memory roguePub = _uncompressedKey(rogue.publicKeyX, rogue.publicKeyY);
        bytes memory sig = _sign(rogue, _commitment());
        assertFalse(_verify(_expectedRootId(), _proofBytes(sig, roguePub)));
    }

    function test_Verify_FalseForRevokedKey() public {
        bytes memory sig = _sign(_commitment());

        vm.prank(owner);
        registry.revokeKey(enclavePubKey);

        assertFalse(_verify(_expectedRootId(), _proofBytes(sig, enclavePubKey)));
    }

    /*//////////////////////////////////////////////////////////////
                           SIGNATURE GATES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseForDifferentSigner() public {
        // Sign with a different key while passing the registered enclave key.
        Vm.Wallet memory rogue = vm.createWallet("rogue");
        bytes memory sig = _sign(rogue, _commitment());
        assertFalse(_verify(_expectedRootId(), _proofBytes(sig, enclavePubKey)));
    }

    function test_Verify_FalseForBadSignatureLength() public {
        bytes memory sig = hex"1234";
        assertFalse(_verify(_expectedRootId(), _proofBytes(sig, enclavePubKey)));
    }

    function test_Verify_FalseForSignatureLength64() public {
        // EIP-2098 "compact" 64-byte signatures are NOT accepted; the
        // contract is strict about 65-byte (r || s || v) tuples.
        bytes memory sig = new bytes(64);
        assertFalse(_verify(_expectedRootId(), _proofBytes(sig, enclavePubKey)));
    }

    function test_Verify_FalseForEmptySignature() public {
        bytes memory sig = "";
        assertFalse(_verify(_expectedRootId(), _proofBytes(sig, enclavePubKey)));
    }

    function test_Verify_FalseForHighSSignature() public {
        // EIP-2 low-s enforcement: even if (r, s) is a valid signature, an
        // s > secp256k1n/2 must be rejected (signature malleability).
        bytes memory sig = _sign(_commitment());
        // Flip s to its high-s equivalent: s' = n - s, v' = v ^ 1.
        bytes32 r;
        bytes32 s;
        uint8 v;
        assembly {
            let p := add(sig, 32)
            r := mload(p)
            s := mload(add(p, 32))
            v := byte(0, mload(add(p, 64)))
        }
        uint256 n = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;
        bytes32 sHigh = bytes32(n - uint256(s));
        uint8 vFlipped = v == 27 ? 28 : 27;
        bytes memory malleable = abi.encodePacked(r, sHigh, vFlipped);
        // Original signature still validates...
        assertTrue(_verify(_expectedRootId(), _proofBytes(sig, enclavePubKey)));
        // ...but the malleable high-s twin must NOT.
        assertFalse(_verify(_expectedRootId(), _proofBytes(malleable, enclavePubKey)));
    }

    function test_Verify_FalseForInvalidV() public {
        // Build a 65-byte signature with v outside {27, 28}.
        bytes memory sig = _sign(_commitment());
        // Overwrite v byte with 29.
        assembly {
            mstore8(add(add(sig, 32), 64), 29)
        }
        assertFalse(_verify(_expectedRootId(), _proofBytes(sig, enclavePubKey)));
        // Also v = 0 (legacy unsigned).
        assembly {
            mstore8(add(add(sig, 32), 64), 0)
        }
        assertFalse(_verify(_expectedRootId(), _proofBytes(sig, enclavePubKey)));
        // Also v = 26.
        assembly {
            mstore8(add(add(sig, 32), 64), 26)
        }
        assertFalse(_verify(_expectedRootId(), _proofBytes(sig, enclavePubKey)));
    }

    function test_Verify_FalseForAllZeroSignature() public {
        // r = s = 0, v = 27. ecrecover returns address(0) → false.
        bytes memory sig = new bytes(65);
        sig[64] = bytes1(uint8(27));
        assertFalse(_verify(_expectedRootId(), _proofBytes(sig, enclavePubKey)));
    }

    /*//////////////////////////////////////////////////////////////
                              KEY GATES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseForCompressedKey() public {
        bytes memory compressed = new bytes(33);
        compressed[0] = 0x02;
        bytes memory sig = _sign(_commitment());
        assertFalse(_verify(_expectedRootId(), _proofBytes(sig, compressed)));
    }

    function test_Verify_FalseForBadKey() public view {
        // 7-byte key cannot be SEC1-decoded → _verifyEnclaveSignature
        // reverts with InvalidPublicKey → verify() catches and returns false.
        bytes memory badKey = hex"01020304050607";
        assertFalse(_verify(_expectedRootId(), _proofBytes(hex"00", badKey)));
    }

    function test_Verify_FalseForEmptyPublicKey() public view {
        bytes memory emptyKey = "";
        assertFalse(_verify(_expectedRootId(), _proofBytes(hex"00", emptyKey)));
    }

    function test_Verify_FalseForKeyWithLength65AndWrongPrefix() public {
        // 65-byte length passes the length gate but the prefix check
        // (`publicKey[0] != 0x04`) must still reject it.
        bytes memory key = new bytes(65);
        key[0] = 0x03;
        // Use a real signature so we fail on the prefix check, not earlier.
        bytes memory sig = _sign(_commitment());
        assertFalse(_verify(_expectedRootId(), _proofBytes(sig, key)));
    }

    /*//////////////////////////////////////////////////////////////
                            ABI DECODE GATES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseForGarbage() public view {
        // Garbage proof bytes that don't decode into the expected tuple must
        // be surfaced as `false` — the ABI decode lives inside the try/catch.
        assertFalse(_verify(bytes32(0), hex"00"));
    }

    function test_Verify_FalseForEmptyProof() public view {
        assertFalse(_verify(bytes32(0), ""));
    }

    function test_Verify_FalseForTruncatedProof() public view {
        // 31 bytes is too short to even decode the first uint256.
        assertFalse(_verify(_expectedRootId(), hex"0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"));
    }

    function test_Verify_AcceptsZeroL2BlockNumber() public {
        // Boundary: l2BlockNumber = 0 must work, since the rootId is
        // recomputed deterministically and the commitment is signed over
        // exactly that value.
        WorldChainProofLib.TransitionPublicValues memory transition = _transition();
        transition.l2PostBlockNumber = 0;
        bytes32 rootId =
            WorldChainProofLib.rootId(DOMAIN_HASH, address(parent), L2_POST_ROOT, 0, L1_ORIGIN_HASH, L1_ORIGIN_NUMBER);
        bytes32 commitment = keccak256(abi.encode(transition));
        bytes memory sig = _sign(commitment);
        bytes memory proof = abi.encode(DOMAIN_HASH, address(parent), L1_ORIGIN_NUMBER, transition, sig, enclavePubKey);
        _setGameContext(transition);
        assertTrue(_verify(rootId, proof));
    }

    function test_Verify_FalseForWrongRollupConfigHash() public {
        // The proof's `rollupConfigHash` participates in the signing
        // commitment but NOT in the rootId reconstruction (see
        // `NitroProofVerifier._decodeAndVerify`). A mismatched
        // rollupConfigHash in the proof must therefore cause the signature
        // recovery to mismatch the expected key and surface as `false`,
        // even though `rootId` still reconstructs correctly.
        bytes32 wrongCfg = keccak256("wrong-cfg");
        WorldChainProofLib.TransitionPublicValues memory wrongTransition = _transition();
        wrongTransition.rollupConfigHash = wrongCfg;
        bytes32 commitment = keccak256(abi.encode(wrongTransition));
        bytes memory sig = _sign(commitment);
        // Build the proof claiming the ORIGINAL rollupConfigHash (so rootId
        // reconstructs to the expected one), but with a signature over the
        // wrong-cfg commitment.
        bytes memory proof =
            abi.encode(DOMAIN_HASH, address(parent), L1_ORIGIN_NUMBER, _transition(), sig, enclavePubKey);
        assertFalse(_verify(_expectedRootId(), proof));
    }

    function test_Verify_PerCallIdempotent() public {
        // verify() is view: calling it twice must return the same result
        // and not record any state change.
        bytes memory sig = _sign(_commitment());
        bytes memory proof = _proofBytes(sig, enclavePubKey);
        bytes32 root = _expectedRootId();
        assertTrue(_verify(root, proof));
        assertTrue(_verify(root, proof));
    }

    /*//////////////////////////////////////////////////////////////
                          INTERNAL ENTRY POINT
    //////////////////////////////////////////////////////////////*/

    function test_DecodeAndVerify_NotCallableExternally() public {
        vm.expectRevert(bytes("internal"));
        proofVerifier._decodeAndVerify(address(game), bytes32(0), hex"00");
    }
}
