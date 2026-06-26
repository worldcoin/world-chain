// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test, Vm} from "forge-std/Test.sol";
import {NitroEnclaveKeyRegistry} from "../../src/proofs/nitro/NitroEnclaveKeyRegistry.sol";
import {NitroProofVerifier} from "../../src/proofs/nitro/NitroProofVerifier.sol";
import {Secp256k1} from "../../src/proofs/nitro/libraries/Secp256k1.sol";
import {WorldChainProofLib} from "../../src/proofs/WorldChainProofLib.sol";
import {MockNitroAttestationVerifier} from "./mocks/MockNitroAttestationVerifier.sol";

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

    // Example boot-info fields used to build a signing commitment.
    bytes32 constant L2_POST_ROOT = keccak256("l2-post-root");
    uint64 constant L2_BLOCK = 123_456;
    bytes32 constant ROLLUP_CFG = keccak256("rollup-cfg");

    // Context fields needed to rebuild rootId in the generic verify() hook.
    bytes32 constant DOMAIN_HASH = keccak256("domain");
    address constant PARENT_REF = address(0xBEEF);
    bytes32 constant INTERMEDIATE_ROOTS = keccak256("intermediate-roots");
    bytes32 constant L1_ORIGIN_HASH = keccak256("l1-origin");
    uint256 constant L1_ORIGIN_NUMBER = 9_001;

    Vm.Wallet enclaveWallet;
    bytes enclavePubKey;
    bytes enclavePubKeyCompressed;

    function setUp() public {
        attestationVerifier = new MockNitroAttestationVerifier();
        registry = new NitroEnclaveKeyRegistry(attestationVerifier, owner);
        proofVerifier = new NitroProofVerifier(registry);

        enclaveWallet = vm.createWallet("enclave");
        enclavePubKey = _uncompressedKey(enclaveWallet.publicKeyX, enclaveWallet.publicKeyY);
        enclavePubKeyCompressed = _compressedKey(enclaveWallet.publicKeyX, enclaveWallet.publicKeyY);

        attestationVerifier.setExpectation(TBS, SIG, PCR0, PCR1, PCR2, enclavePubKey);
        registry.registerKey(TBS, SIG, PCR0, PCR1, PCR2);
    }

    function _uncompressedKey(uint256 x, uint256 y) internal pure returns (bytes memory out) {
        out = new bytes(65);
        out[0] = 0x04;
        assembly {
            mstore(add(out, 33), x)
            mstore(add(out, 65), y)
        }
    }

    function _compressedKey(uint256 x, uint256 y) internal pure returns (bytes memory out) {
        out = new bytes(33);
        out[0] = (y & 1 == 1) ? bytes1(0x03) : bytes1(0x02);
        assembly {
            mstore(add(out, 33), x)
        }
    }

    function _sign(bytes32 digest) internal returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(enclaveWallet, digest);
        return abi.encodePacked(r, s, v);
    }

    function _commitment() internal view returns (bytes32) {
        return proofVerifier.signingCommitment(L2_POST_ROOT, L2_BLOCK, ROLLUP_CFG);
    }

    /*//////////////////////////////////////////////////////////////
                           SIGNING COMMITMENT
    //////////////////////////////////////////////////////////////*/

    function test_SigningCommitment_MatchesRustEncoding() public view {
        // signing_commitment(boot_info) = keccak256( l2PostRoot ||
        //   uint64BE(l2BlockNumber) || rollupConfigHash )
        bytes memory packed = abi.encodePacked(L2_POST_ROOT, L2_BLOCK, ROLLUP_CFG);
        assertEq(packed.length, 32 + 8 + 32);
        assertEq(keccak256(packed), proofVerifier.signingCommitment(L2_POST_ROOT, L2_BLOCK, ROLLUP_CFG));
    }

    /*//////////////////////////////////////////////////////////////
                          verifyProof (typed)
    //////////////////////////////////////////////////////////////*/

    function test_VerifyProof_HappyPath() public {
        bytes32 commitment = _commitment();
        bytes memory sig = _sign(commitment);
        assertTrue(proofVerifier.verifyProof(L2_POST_ROOT, L2_BLOCK, ROLLUP_CFG, sig, enclavePubKey));
    }

    function test_VerifyProof_AcceptsCompressedKey() public {
        bytes32 commitment = _commitment();
        bytes memory sig = _sign(commitment);
        assertTrue(proofVerifier.verifyProof(L2_POST_ROOT, L2_BLOCK, ROLLUP_CFG, sig, enclavePubKeyCompressed));
    }

    function test_VerifyProof_FalseForUnregisteredKey() public {
        Vm.Wallet memory rogue = vm.createWallet("rogue");
        bytes memory roguePub = _uncompressedKey(rogue.publicKeyX, rogue.publicKeyY);
        bytes32 commitment = _commitment();
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(rogue, commitment);
        bytes memory sig = abi.encodePacked(r, s, v);
        assertFalse(proofVerifier.verifyProof(L2_POST_ROOT, L2_BLOCK, ROLLUP_CFG, sig, roguePub));
    }

    function test_VerifyProof_FalseForRevokedKey() public {
        bytes32 commitment = _commitment();
        bytes memory sig = _sign(commitment);

        vm.prank(owner);
        registry.revokeKey(enclavePubKey);

        assertFalse(proofVerifier.verifyProof(L2_POST_ROOT, L2_BLOCK, ROLLUP_CFG, sig, enclavePubKey));
    }

    function test_VerifyProof_FalseForDifferentSigner() public {
        Vm.Wallet memory rogue = vm.createWallet("rogue");
        bytes32 commitment = _commitment();
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(rogue, commitment);
        bytes memory sig = abi.encodePacked(r, s, v);
        assertFalse(proofVerifier.verifyProof(L2_POST_ROOT, L2_BLOCK, ROLLUP_CFG, sig, enclavePubKey));
    }

    function test_VerifyProof_FalseForWrongBootInfo() public {
        // Signature over the correct commitment is replayed against a different
        // block number — the recomputed digest will not match.
        bytes32 commitment = _commitment();
        bytes memory sig = _sign(commitment);
        assertFalse(proofVerifier.verifyProof(L2_POST_ROOT, L2_BLOCK + 1, ROLLUP_CFG, sig, enclavePubKey));
    }

    function test_VerifyProof_RevertsForBadSignatureLength() public {
        bytes memory sig = hex"1234";
        vm.expectRevert(NitroProofVerifier.InvalidSignatureLength.selector);
        proofVerifier.verifyProof(L2_POST_ROOT, L2_BLOCK, ROLLUP_CFG, sig, enclavePubKey);
    }

    /*//////////////////////////////////////////////////////////////
                       verifyProofPrecomputed
    //////////////////////////////////////////////////////////////*/

    function test_VerifyProofPrecomputed_HappyPath() public {
        bytes32 commitment = _commitment();
        bytes memory sig = _sign(commitment);
        assertTrue(proofVerifier.verifyProofPrecomputed(commitment, sig, enclavePubKey));
    }

    /*//////////////////////////////////////////////////////////////
                       verify (generic interface)
    //////////////////////////////////////////////////////////////*/

    function _proofBytes(bytes memory sig, bytes memory pub) internal pure returns (bytes memory) {
        return abi.encode(
            DOMAIN_HASH,
            PARENT_REF,
            INTERMEDIATE_ROOTS,
            L1_ORIGIN_HASH,
            L1_ORIGIN_NUMBER,
            ROLLUP_CFG,
            L2_POST_ROOT,
            L2_BLOCK,
            sig,
            pub
        );
    }

    function _expectedRootId() internal pure returns (bytes32) {
        return WorldChainProofLib.rootId(
            DOMAIN_HASH,
            PARENT_REF,
            L2_POST_ROOT,
            uint256(L2_BLOCK),
            INTERMEDIATE_ROOTS,
            L1_ORIGIN_HASH,
            L1_ORIGIN_NUMBER
        );
    }

    function test_Verify_GenericInterface() public {
        bytes32 commitment = _commitment();
        bytes memory sig = _sign(commitment);
        assertTrue(proofVerifier.verify(_expectedRootId(), _proofBytes(sig, enclavePubKey)));
    }

    function test_Verify_GenericInterface_AcceptsCompressedKey() public {
        bytes32 commitment = _commitment();
        bytes memory sig = _sign(commitment);
        assertTrue(proofVerifier.verify(_expectedRootId(), _proofBytes(sig, enclavePubKeyCompressed)));
    }

    function test_Verify_GenericInterface_FalseForWrongRootId() public {
        // Honest signature + boot_info, but the game asks about a different rootId
        // — the verifier must NOT validate.
        bytes32 commitment = _commitment();
        bytes memory sig = _sign(commitment);
        assertFalse(proofVerifier.verify(bytes32(uint256(0xdead)), _proofBytes(sig, enclavePubKey)));
    }

    function test_Verify_GenericInterface_FalseOnRevert() public {
        // Malformed signature → verifyProof reverts → verify returns false.
        assertFalse(proofVerifier.verify(_expectedRootId(), _proofBytes(hex"1234", enclavePubKey)));
    }

    function test_Verify_GenericInterface_FalseForGarbage() public view {
        // Garbage proof bytes that don't decode into the expected tuple must
        // be surfaced as `false` — the ABI decode lives inside the try/catch.
        assertFalse(proofVerifier.verify(bytes32(0), hex"00"));
    }

    function test_Verify_GenericInterface_FalseForEmptyProof() public view {
        assertFalse(proofVerifier.verify(bytes32(0), ""));
    }

    function test_Verify_GenericInterface_FalseForBadKey() public view {
        // 7-byte key cannot be SEC1-decoded → Secp256k1.normalizeToUncompressed
        // reverts → verify() returns false. We must still supply a valid rootId
        // so the binding check doesn't short-circuit before reaching the key.
        bytes memory badKey = hex"01020304050607";
        assertFalse(proofVerifier.verify(_expectedRootId(), _proofBytes(hex"00", badKey)));
    }

    function test_DecodeAndVerify_NotCallableExternally() public {
        vm.expectRevert(bytes("internal"));
        proofVerifier._decodeAndVerify(bytes32(0), hex"00");
    }
}
