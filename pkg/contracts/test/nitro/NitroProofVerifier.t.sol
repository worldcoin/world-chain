// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test, Vm} from "forge-std/Test.sol";
import {NitroEnclaveKeyRegistry} from "../../src/proofs/nitro/NitroEnclaveKeyRegistry.sol";
import {NitroProofVerifier} from "../../src/proofs/nitro/NitroProofVerifier.sol";
import {ProofLib} from "../../src/proofs/lib/ProofLib.sol";
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

    bytes32 constant ROOT_ID = keccak256("root-id");
    bytes32 constant L1_ORIGIN_HASH = keccak256("l1-origin");
    bytes32 constant L2_PRE_ROOT = keccak256("l2-pre-root");
    uint64 constant L2_PRE_BLOCK = 123_455;
    bytes32 constant L2_POST_ROOT = keccak256("l2-post-root");
    uint64 constant L2_BLOCK = 123_456;
    bytes32 constant ROLLUP_CFG = keccak256("rollup-cfg");

    Vm.Wallet enclaveWallet;
    bytes enclavePubKey;

    function setUp() public {
        attestationVerifier = new MockNitroAttestationVerifier();
        registry = new NitroEnclaveKeyRegistry(attestationVerifier, owner);
        proofVerifier = new NitroProofVerifier(registry);

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

    function _transition() internal pure returns (ProofLib.TransitionPublicValues memory) {
        return ProofLib.TransitionPublicValues({
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

    function _verify(bytes memory sig) internal view returns (bool) {
        return proofVerifier.verify(ROOT_ID, _transition(), sig);
    }

    /*//////////////////////////////////////////////////////////////
                              HAPPY PATH
    //////////////////////////////////////////////////////////////*/

    function test_Verify_HappyPath() public {
        assertTrue(_verify(_sign(_commitment())));
    }

    function test_Verify_AcceptsZeroL2BlockNumber() public {
        ProofLib.TransitionPublicValues memory transition = _transition();
        transition.l2PostBlockNumber = 0;
        bytes memory sig = _sign(keccak256(abi.encode(transition)));

        assertTrue(proofVerifier.verify(ROOT_ID, transition, sig));
    }

    function test_Verify_PerCallIdempotent() public {
        bytes memory sig = _sign(_commitment());

        assertTrue(_verify(sig));
        assertTrue(_verify(sig));
    }

    /*//////////////////////////////////////////////////////////////
                            BINDING FAILURES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseWhenSignatureAttestsDifferentTransition() public {
        ProofLib.TransitionPublicValues memory proven = _transition();
        proven.l2PreRoot = keccak256("wrong-pre-root");
        bytes memory sig = _sign(keccak256(abi.encode(proven)));

        assertFalse(_verify(sig));
    }

    function test_Verify_FalseForWrongRollupConfigHash() public {
        ProofLib.TransitionPublicValues memory proven = _transition();
        proven.rollupConfigHash = keccak256("wrong-cfg");
        bytes memory sig = _sign(keccak256(abi.encode(proven)));

        assertFalse(_verify(sig));
    }

    /*//////////////////////////////////////////////////////////////
                            REGISTRY GATES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseForUnregisteredSigner() public {
        Vm.Wallet memory rogue = vm.createWallet("rogue");

        assertFalse(_verify(_sign(rogue, _commitment())));
    }

    function test_Verify_FalseForRevokedSigner() public {
        bytes memory sig = _sign(_commitment());

        vm.prank(owner);
        registry.revokeSigner(enclaveWallet.addr);

        assertFalse(_verify(sig));
    }

    /*//////////////////////////////////////////////////////////////
                           SIGNATURE GATES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseForBadSignatureLength() public view {
        assertFalse(_verify(hex"1234"));
    }

    function test_Verify_FalseForSignatureLength64() public view {
        assertFalse(_verify(new bytes(64)));
    }

    function test_Verify_FalseForEmptySignature() public view {
        assertFalse(_verify(""));
    }

    function test_Verify_FalseForHighSSignature() public {
        bytes memory sig = _sign(_commitment());
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

        assertTrue(_verify(sig));
        assertFalse(_verify(malleable));
    }

    function test_Verify_FalseForInvalidV() public {
        bytes memory sig = _sign(_commitment());
        assembly {
            mstore8(add(add(sig, 32), 64), 29)
        }
        assertFalse(_verify(sig));
        assembly {
            mstore8(add(add(sig, 32), 64), 0)
        }
        assertFalse(_verify(sig));
        assembly {
            mstore8(add(add(sig, 32), 64), 26)
        }
        assertFalse(_verify(sig));
    }

    function test_Verify_FalseForAllZeroSignature() public view {
        bytes memory sig = new bytes(65);
        sig[64] = bytes1(uint8(27));

        assertFalse(_verify(sig));
    }
}
