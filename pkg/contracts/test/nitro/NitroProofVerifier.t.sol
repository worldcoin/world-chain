// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test, Vm} from "forge-std/Test.sol";
import {NitroEnclaveKeyRegistry} from "../../src/proofs/nitro/NitroEnclaveKeyRegistry.sol";
import {NitroProofVerifier} from "../../src/proofs/nitro/NitroProofVerifier.sol";
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

    Vm.Wallet enclaveWallet;
    bytes enclavePubKey;

    function setUp() public {
        attestationVerifier = new MockNitroAttestationVerifier();
        registry = new NitroEnclaveKeyRegistry(attestationVerifier, owner);
        proofVerifier = new NitroProofVerifier(registry);

        enclaveWallet = vm.createWallet("enclave");
        enclavePubKey = _uncompressedKey(enclaveWallet.publicKeyX, enclaveWallet.publicKeyY);

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

    function _sign(bytes32 digest) internal returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(enclaveWallet, digest);
        return abi.encodePacked(r, s, v);
    }

    function test_VerifyProof_HappyPath() public {
        bytes32 commitment = keccak256("commit");
        bytes memory sig = _sign(commitment);
        assertTrue(proofVerifier.verifyProof(commitment, sig, enclavePubKey));
    }

    function test_VerifyProof_FalseForUnregisteredKey() public {
        Vm.Wallet memory rogue = vm.createWallet("rogue");
        bytes memory roguePub = _uncompressedKey(rogue.publicKeyX, rogue.publicKeyY);
        bytes32 commitment = keccak256("commit");
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(rogue, commitment);
        bytes memory sig = abi.encodePacked(r, s, v);
        assertFalse(proofVerifier.verifyProof(commitment, sig, roguePub));
    }

    function test_VerifyProof_FalseForRevokedKey() public {
        bytes32 commitment = keccak256("commit");
        bytes memory sig = _sign(commitment);

        vm.prank(owner);
        registry.revokeKey(enclavePubKey);

        assertFalse(proofVerifier.verifyProof(commitment, sig, enclavePubKey));
    }

    function test_VerifyProof_FalseForDifferentSigner() public {
        // Sign with a different key while passing the registered enclave key.
        Vm.Wallet memory rogue = vm.createWallet("rogue");
        bytes32 commitment = keccak256("commit");
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(rogue, commitment);
        bytes memory sig = abi.encodePacked(r, s, v);
        assertFalse(proofVerifier.verifyProof(commitment, sig, enclavePubKey));
    }

    function test_VerifyProof_RevertsForBadSignatureLength() public {
        bytes32 commitment = keccak256("commit");
        bytes memory sig = hex"1234";
        vm.expectRevert(NitroProofVerifier.InvalidSignatureLength.selector);
        proofVerifier.verifyProof(commitment, sig, enclavePubKey);
    }

    function test_Verify_GenericInterface() public {
        bytes32 commitment = keccak256("commit");
        bytes memory sig = _sign(commitment);
        bytes memory proof = abi.encode(sig, enclavePubKey);
        assertTrue(proofVerifier.verify(commitment, proof));
    }

    function test_Verify_GenericInterface_FalseOnRevert() public {
        bytes32 commitment = keccak256("commit");
        // Malformed signature → verifyProof reverts → verify returns false.
        bytes memory proof = abi.encode(hex"1234", enclavePubKey);
        assertFalse(proofVerifier.verify(commitment, proof));
    }

    function test_Verify_GenericInterface_FalseForGarbage() public view {
        // Garbage proof bytes that don't decode into (bytes, bytes) must be
        // surfaced as `false` — the ABI decode lives inside the try/catch so
        // verify() never reverts on malformed input.
        assertFalse(proofVerifier.verify(bytes32(0), hex"00"));
    }

    function test_Verify_GenericInterface_FalseForEmptyProof() public view {
        assertFalse(proofVerifier.verify(bytes32(0), ""));
    }

    function test_DecodeAndVerify_NotCallableExternally() public {
        // The helper must reject direct calls so it can only be invoked via verify().
        vm.expectRevert(bytes("internal"));
        proofVerifier._decodeAndVerify(bytes32(0), hex"00");
    }
}
