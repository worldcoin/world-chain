// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";

import {ISP1Verifier} from "@sp1-contracts/src/ISP1Verifier.sol";
import {AggregationPublicValues, SP1ValidityVerifier} from "../../src/proofs/sp1/SP1ValidityVerifier.sol";
import {LibProof} from "../../src/proofs/lib/LibProof.sol";

contract StubSP1Verifier is ISP1Verifier {
    bool public reject;
    bytes32 public expectedProgramVKey;
    bytes32 public expectedPublicValuesHash;
    bytes32 public expectedProofBytesHash;

    function setReject(bool reject_) external {
        reject = reject_;
    }

    function setExpectation(bytes32 programVKey, bytes calldata publicValues, bytes calldata proofBytes) external {
        expectedProgramVKey = programVKey;
        expectedPublicValuesHash = keccak256(publicValues);
        expectedProofBytesHash = keccak256(proofBytes);
    }

    function verifyProof(bytes32 programVKey, bytes calldata publicValues, bytes calldata proofBytes) external view {
        require(!reject, "stub: invalid proof");
        require(programVKey == expectedProgramVKey, "stub: program vkey mismatch");
        require(keccak256(publicValues) == expectedPublicValuesHash, "stub: public values mismatch");
        require(keccak256(proofBytes) == expectedProofBytesHash, "stub: proof bytes mismatch");
    }
}

contract SP1ValidityVerifierTest is Test {
    StubSP1Verifier internal sp1;
    SP1ValidityVerifier internal verifier;

    bytes32 internal constant AGGREGATION_VKEY = bytes32(uint256(0xA66));
    bytes32 internal constant ROLLUP_CONFIG_HASH = keccak256("world-chain-rollup-config");
    bytes32 internal constant RANGE_VKEY_COMMITMENT = keccak256("range-vkey");

    bytes32 internal constant ROOT_ID = keccak256("root-id");
    bytes32 internal constant L1_ORIGIN_HASH = keccak256("l1-origin");
    bytes32 internal constant L2_PRE_ROOT = keccak256("l2-pre-root");
    bytes32 internal constant L2_POST_ROOT = keccak256("l2-post-root");
    uint64 internal constant L2_PRE_BLOCK_NUMBER = 123_455;
    uint64 internal constant L2_BLOCK_NUMBER = 123_456;

    bytes internal constant SP1_PROOF_BYTES = hex"4388a21cdeadbeef";

    function setUp() public {
        sp1 = new StubSP1Verifier();
        verifier = new SP1ValidityVerifier(ISP1Verifier(address(sp1)), AGGREGATION_VKEY, RANGE_VKEY_COMMITMENT);
    }

    /*//////////////////////////////////////////////////////////////
                                HELPERS
    //////////////////////////////////////////////////////////////*/

    function _transition() internal pure returns (LibProof.TransitionPublicValues memory) {
        return LibProof.TransitionPublicValues({
            l1Head: L1_ORIGIN_HASH,
            l2PreRoot: L2_PRE_ROOT,
            l2PreBlockNumber: L2_PRE_BLOCK_NUMBER,
            l2PostRoot: L2_POST_ROOT,
            l2PostBlockNumber: L2_BLOCK_NUMBER,
            rollupConfigHash: ROLLUP_CONFIG_HASH
        });
    }

    function _publicValues(LibProof.TransitionPublicValues memory transition, bytes32 multiBlockVKey)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encode(AggregationPublicValues({transitionPublicValues: transition, multiBlockVKey: multiBlockVKey}));
    }

    function _expectSp1Call(LibProof.TransitionPublicValues memory transition) internal {
        sp1.setExpectation(AGGREGATION_VKEY, _publicValues(transition, RANGE_VKEY_COMMITMENT), SP1_PROOF_BYTES);
    }

    /*//////////////////////////////////////////////////////////////
                               CONSTRUCTOR
    //////////////////////////////////////////////////////////////*/

    function test_Constructor_RevertsForZeroSP1Verifier() public {
        vm.expectRevert(SP1ValidityVerifier.ZeroSP1Verifier.selector);
        new SP1ValidityVerifier(ISP1Verifier(address(0)), AGGREGATION_VKEY, RANGE_VKEY_COMMITMENT);
    }

    function test_Constructor_RevertsForZeroAggregationVKey() public {
        vm.expectRevert(SP1ValidityVerifier.ZeroAggregationVKey.selector);
        new SP1ValidityVerifier(ISP1Verifier(address(sp1)), bytes32(0), RANGE_VKEY_COMMITMENT);
    }

    function test_Constructor_RevertsForZeroRangeVKeyCommitment() public {
        vm.expectRevert(SP1ValidityVerifier.ZeroRangeVKeyCommitment.selector);
        new SP1ValidityVerifier(ISP1Verifier(address(sp1)), AGGREGATION_VKEY, bytes32(0));
    }

    /*//////////////////////////////////////////////////////////////
                                 VERIFY
    //////////////////////////////////////////////////////////////*/

    function test_Verify_HappyPath() public {
        LibProof.TransitionPublicValues memory transition = _transition();
        _expectSp1Call(transition);

        assertTrue(verifier.verify(ROOT_ID, transition, SP1_PROOF_BYTES));
    }

    function test_Verify_FalseWhenSP1ProofInvalid() public {
        LibProof.TransitionPublicValues memory transition = _transition();
        _expectSp1Call(transition);
        sp1.setReject(true);

        assertFalse(verifier.verify(ROOT_ID, transition, SP1_PROOF_BYTES));
    }

    function test_Verify_FalseForUnexpectedProofBytes() public {
        LibProof.TransitionPublicValues memory transition = _transition();
        _expectSp1Call(transition);

        assertFalse(verifier.verify(ROOT_ID, transition, hex"deadbeef"));
    }

    function test_Verify_FalseWhenProofAttestsDifferentTransition() public {
        LibProof.TransitionPublicValues memory proven = _transition();
        _expectSp1Call(proven);

        LibProof.TransitionPublicValues memory expected = _transition();
        expected.l2PostRoot = keccak256("other-post-root");

        assertFalse(verifier.verify(ROOT_ID, expected, SP1_PROOF_BYTES));
    }

    function test_Verify_BindsRangeVKeyCommitment() public {
        LibProof.TransitionPublicValues memory transition = _transition();
        sp1.setExpectation(AGGREGATION_VKEY, _publicValues(transition, keccak256("wrong-range-vkey")), SP1_PROOF_BYTES);

        assertFalse(verifier.verify(ROOT_ID, transition, SP1_PROOF_BYTES));
    }
}
