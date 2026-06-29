// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test, Vm} from "forge-std/Test.sol";
import {NitroEnclaveKeyRegistry} from "../../src/proofs/nitro/NitroEnclaveKeyRegistry.sol";
import {NitroProofVerifier} from "../../src/proofs/nitro/NitroProofVerifier.sol";
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

    // Boot-info fields used to build a signing commitment.
    bytes32 constant L2_POST_ROOT = keccak256("l2-post-root");
    uint64 constant L2_BLOCK = 123_456;
    bytes32 constant ROLLUP_CFG = keccak256("rollup-cfg");

    // Context fields needed to rebuild rootId.
    bytes32 constant DOMAIN_HASH = keccak256("domain");
    address constant PARENT_REF = address(0xBEEF);
    bytes32 constant INTERMEDIATE_ROOTS = keccak256("intermediate-roots");
    bytes32 constant L1_ORIGIN_HASH = keccak256("l1-origin");
    uint256 constant L1_ORIGIN_NUMBER = 9_001;

    Vm.Wallet enclaveWallet;
    bytes enclavePubKey;

    function setUp() public {
        attestationVerifier = new MockNitroAttestationVerifier();
        registry = new NitroEnclaveKeyRegistry(attestationVerifier, owner);
        proofVerifier = new NitroProofVerifier(registry);

        enclaveWallet = vm.createWallet("enclave");
        enclavePubKey = _uncompressedKey(enclaveWallet.publicKeyX, enclaveWallet.publicKeyY);

        attestationVerifier.setExpectation(TBS, SIG, enclavePubKey, PCR0, PCR1, PCR2);
        registry.registerKey(TBS, SIG);
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

    function _commitment() internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(L2_POST_ROOT, L2_BLOCK, ROLLUP_CFG));
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

    /*//////////////////////////////////////////////////////////////
                              HAPPY PATH
    //////////////////////////////////////////////////////////////*/

    function test_Verify_HappyPath() public {
        bytes memory sig = _sign(_commitment());
        assertTrue(proofVerifier.verify(_expectedRootId(), _proofBytes(sig, enclavePubKey)));
    }

    /*//////////////////////////////////////////////////////////////
                            BINDING FAILURES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseForWrongRootId() public {
        // Honest signature + boot_info, but the game asks about a different
        // rootId — the verifier must NOT validate.
        bytes memory sig = _sign(_commitment());
        assertFalse(proofVerifier.verify(bytes32(uint256(0xdead)), _proofBytes(sig, enclavePubKey)));
    }

    function test_Verify_FalseForWrongBootInfo() public {
        // The proof claims (L2_POST_ROOT, L2_BLOCK + 1, ROLLUP_CFG) but the
        // signature is over the L2_BLOCK commitment — rootId check still
        // passes for the modified block number? It must not: the signing
        // commitment is recomputed from the proof's boot_info, so a wrong
        // commitment surfaces as a signature mismatch.
        bytes memory sig = _sign(_commitment());
        // Build a proof with a mismatched block number and a matching rootId.
        bytes32 wrongRootId = WorldChainProofLib.rootId(
            DOMAIN_HASH,
            PARENT_REF,
            L2_POST_ROOT,
            uint256(L2_BLOCK + 1),
            INTERMEDIATE_ROOTS,
            L1_ORIGIN_HASH,
            L1_ORIGIN_NUMBER
        );
        bytes memory proof = abi.encode(
            DOMAIN_HASH,
            PARENT_REF,
            INTERMEDIATE_ROOTS,
            L1_ORIGIN_HASH,
            L1_ORIGIN_NUMBER,
            ROLLUP_CFG,
            L2_POST_ROOT,
            L2_BLOCK + 1,
            sig,
            enclavePubKey
        );
        assertFalse(proofVerifier.verify(wrongRootId, proof));
    }

    /*//////////////////////////////////////////////////////////////
                            REGISTRY GATES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseForUnregisteredKey() public {
        Vm.Wallet memory rogue = vm.createWallet("rogue");
        bytes memory roguePub = _uncompressedKey(rogue.publicKeyX, rogue.publicKeyY);
        bytes memory sig = _sign(rogue, _commitment());
        assertFalse(proofVerifier.verify(_expectedRootId(), _proofBytes(sig, roguePub)));
    }

    function test_Verify_FalseForRevokedKey() public {
        bytes memory sig = _sign(_commitment());

        vm.prank(owner);
        registry.revokeKey(enclavePubKey);

        assertFalse(proofVerifier.verify(_expectedRootId(), _proofBytes(sig, enclavePubKey)));
    }

    /*//////////////////////////////////////////////////////////////
                           SIGNATURE GATES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseForDifferentSigner() public {
        // Sign with a different key while passing the registered enclave key.
        Vm.Wallet memory rogue = vm.createWallet("rogue");
        bytes memory sig = _sign(rogue, _commitment());
        assertFalse(proofVerifier.verify(_expectedRootId(), _proofBytes(sig, enclavePubKey)));
    }

    function test_Verify_FalseForBadSignatureLength() public {
        bytes memory sig = hex"1234";
        assertFalse(proofVerifier.verify(_expectedRootId(), _proofBytes(sig, enclavePubKey)));
    }

    /*//////////////////////////////////////////////////////////////
                              KEY GATES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseForCompressedKey() public {
        bytes memory compressed = new bytes(33);
        compressed[0] = 0x02;
        bytes memory sig = _sign(_commitment());
        assertFalse(proofVerifier.verify(_expectedRootId(), _proofBytes(sig, compressed)));
    }

    function test_Verify_FalseForBadKey() public view {
        // 7-byte key cannot be SEC1-decoded → _verifyEnclaveSignature
        // reverts with InvalidPublicKey → verify() catches and returns false.
        bytes memory badKey = hex"01020304050607";
        assertFalse(proofVerifier.verify(_expectedRootId(), _proofBytes(hex"00", badKey)));
    }

    /*//////////////////////////////////////////////////////////////
                            ABI DECODE GATES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseForGarbage() public view {
        // Garbage proof bytes that don't decode into the expected tuple must
        // be surfaced as `false` — the ABI decode lives inside the try/catch.
        assertFalse(proofVerifier.verify(bytes32(0), hex"00"));
    }

    function test_Verify_FalseForEmptyProof() public view {
        assertFalse(proofVerifier.verify(bytes32(0), ""));
    }

    /*//////////////////////////////////////////////////////////////
                          INTERNAL ENTRY POINT
    //////////////////////////////////////////////////////////////*/

    function test_DecodeAndVerify_NotCallableExternally() public {
        vm.expectRevert(bytes("internal"));
        proofVerifier._decodeAndVerify(bytes32(0), hex"00");
    }
}
