// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";

import {ISP1Verifier} from "@sp1-contracts/src/ISP1Verifier.sol";
import {AggregationOutputs, SP1ValidityVerifier} from "../src/proofs/SP1ValidityVerifier.sol";
import {WorldChainProofLib} from "../src/proofs/WorldChainProofLib.sol";

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

    bytes32 internal constant DOMAIN_HASH = keccak256("domain");
    address internal constant PARENT_REF = address(0xBEEF);
    bytes32 internal constant INTERMEDIATE_ROOTS_HASH = keccak256("intermediate-roots");
    bytes32 internal constant L1_ORIGIN_HASH = keccak256("l1-origin");
    uint256 internal constant L1_ORIGIN_NUMBER = 9_001;

    bytes32 internal constant L2_PRE_ROOT = keccak256("l2-pre-root");
    bytes32 internal constant L2_POST_ROOT = keccak256("l2-post-root");
    uint64 internal constant L2_BLOCK_NUMBER = 123_456;
    address internal constant PROVER = address(0xCAFE);

    bytes internal constant SP1_PROOF_BYTES = hex"4388a21cdeadbeef";

    function setUp() public {
        sp1 = new StubSP1Verifier();
        verifier = new SP1ValidityVerifier(
            ISP1Verifier(address(sp1)), AGGREGATION_VKEY, ROLLUP_CONFIG_HASH, RANGE_VKEY_COMMITMENT
        );
    }

    /*//////////////////////////////////////////////////////////////
                                HELPERS
    //////////////////////////////////////////////////////////////*/

    function _outputs() internal pure returns (AggregationOutputs memory) {
        return AggregationOutputs({
            l1Head: L1_ORIGIN_HASH,
            l2PreRoot: L2_PRE_ROOT,
            l2PostRoot: L2_POST_ROOT,
            l2BlockNumber: L2_BLOCK_NUMBER,
            rollupConfigHash: ROLLUP_CONFIG_HASH,
            multiBlockVKey: RANGE_VKEY_COMMITMENT,
            proverAddress: PROVER
        });
    }

    function _publicValues(AggregationOutputs memory outputs) internal pure returns (bytes memory) {
        return abi.encode(outputs);
    }

    function _proof(AggregationOutputs memory outputs) internal pure returns (bytes memory) {
        return _proof(DOMAIN_HASH, PARENT_REF, INTERMEDIATE_ROOTS_HASH, L1_ORIGIN_NUMBER, outputs, SP1_PROOF_BYTES);
    }

    function _proof(
        bytes32 domainHash,
        address parentRef,
        bytes32 intermediateRootsHash,
        uint256 l1OriginNumber,
        AggregationOutputs memory outputs,
        bytes memory proofBytes
    ) internal pure returns (bytes memory) {
        return abi.encode(
            domainHash, parentRef, intermediateRootsHash, l1OriginNumber, _publicValues(outputs), proofBytes
        );
    }

    function _rootId() internal pure returns (bytes32) {
        return WorldChainProofLib.rootId(
            DOMAIN_HASH,
            PARENT_REF,
            L2_POST_ROOT,
            uint256(L2_BLOCK_NUMBER),
            INTERMEDIATE_ROOTS_HASH,
            L1_ORIGIN_HASH,
            L1_ORIGIN_NUMBER
        );
    }

    function _expectSp1Call(AggregationOutputs memory outputs) internal {
        sp1.setExpectation(AGGREGATION_VKEY, _publicValues(outputs), SP1_PROOF_BYTES);
    }

    /*//////////////////////////////////////////////////////////////
                               CONSTRUCTOR
    //////////////////////////////////////////////////////////////*/

    function test_Constructor_RevertsForZeroSP1Verifier() public {
        vm.expectRevert(SP1ValidityVerifier.ZeroSP1Verifier.selector);
        new SP1ValidityVerifier(ISP1Verifier(address(0)), AGGREGATION_VKEY, ROLLUP_CONFIG_HASH, RANGE_VKEY_COMMITMENT);
    }

    function test_Constructor_RevertsForZeroAggregationVKey() public {
        vm.expectRevert(SP1ValidityVerifier.ZeroAggregationVKey.selector);
        new SP1ValidityVerifier(ISP1Verifier(address(sp1)), bytes32(0), ROLLUP_CONFIG_HASH, RANGE_VKEY_COMMITMENT);
    }

    function test_Constructor_RevertsForZeroRollupConfigHash() public {
        vm.expectRevert(SP1ValidityVerifier.ZeroRollupConfigHash.selector);
        new SP1ValidityVerifier(ISP1Verifier(address(sp1)), AGGREGATION_VKEY, bytes32(0), RANGE_VKEY_COMMITMENT);
    }

    function test_Constructor_RevertsForZeroRangeVKeyCommitment() public {
        vm.expectRevert(SP1ValidityVerifier.ZeroRangeVKeyCommitment.selector);
        new SP1ValidityVerifier(ISP1Verifier(address(sp1)), AGGREGATION_VKEY, ROLLUP_CONFIG_HASH, bytes32(0));
    }

    /*//////////////////////////////////////////////////////////////
                              HAPPY PATH
    //////////////////////////////////////////////////////////////*/

    function test_Verify_HappyPath() public {
        AggregationOutputs memory outputs = _outputs();
        _expectSp1Call(outputs);

        assertTrue(verifier.verify(_rootId(), _proof(outputs)));
    }

    /*//////////////////////////////////////////////////////////////
                            DECODE FAILURES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseForMalformedOuterProof() public view {
        assertFalse(verifier.verify(_rootId(), hex"deadbeef"));
    }

    function test_Verify_FalseForMalformedPublicValues() public view {
        bytes memory proof = abi.encode(
            DOMAIN_HASH, PARENT_REF, INTERMEDIATE_ROOTS_HASH, L1_ORIGIN_NUMBER, hex"deadbeef", SP1_PROOF_BYTES
        );

        assertFalse(verifier.verify(_rootId(), proof));
    }

    /*//////////////////////////////////////////////////////////////
                            SP1 GATEWAY FAILURES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseWhenSP1ProofInvalid() public {
        AggregationOutputs memory outputs = _outputs();
        _expectSp1Call(outputs);
        sp1.setReject(true);

        assertFalse(verifier.verify(_rootId(), _proof(outputs)));
    }

    function test_Verify_FalseWhenUnexpectedProofBytesForwarded() public {
        AggregationOutputs memory outputs = _outputs();
        sp1.setExpectation(AGGREGATION_VKEY, _publicValues(outputs), bytes("unexpected"));

        assertFalse(verifier.verify(_rootId(), _proof(outputs)));
    }

    /*//////////////////////////////////////////////////////////////
                            PUBLIC VALUE GATES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseForRollupConfigHashMismatch() public view {
        AggregationOutputs memory outputs = _outputs();
        outputs.rollupConfigHash = keccak256("wrong-rollup-config");

        assertFalse(verifier.verify(_rootId(), _proof(outputs)));
    }

    function test_Verify_FalseForRangeVKeyMismatch() public view {
        AggregationOutputs memory outputs = _outputs();
        outputs.multiBlockVKey = keccak256("wrong-range-vkey");

        assertFalse(verifier.verify(_rootId(), _proof(outputs)));
    }

    /*//////////////////////////////////////////////////////////////
                             ROOT ID GATES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseForWrongRootId() public {
        AggregationOutputs memory outputs = _outputs();
        _expectSp1Call(outputs);

        assertFalse(verifier.verify(bytes32(uint256(0xdead)), _proof(outputs)));
    }

    function test_Verify_FalseForDomainHashMismatch() public view {
        AggregationOutputs memory outputs = _outputs();
        bytes memory proof = _proof(
            keccak256("wrong-domain"), PARENT_REF, INTERMEDIATE_ROOTS_HASH, L1_ORIGIN_NUMBER, outputs, SP1_PROOF_BYTES
        );

        assertFalse(verifier.verify(_rootId(), proof));
    }

    function test_Verify_FalseForParentRefMismatch() public view {
        AggregationOutputs memory outputs = _outputs();
        bytes memory proof =
            _proof(DOMAIN_HASH, address(0xBAD), INTERMEDIATE_ROOTS_HASH, L1_ORIGIN_NUMBER, outputs, SP1_PROOF_BYTES);

        assertFalse(verifier.verify(_rootId(), proof));
    }

    function test_Verify_FalseForPostRootMismatch() public view {
        AggregationOutputs memory outputs = _outputs();
        outputs.l2PostRoot = keccak256("wrong-post-root");

        assertFalse(verifier.verify(_rootId(), _proof(outputs)));
    }

    function test_Verify_FalseForBlockNumberMismatch() public view {
        AggregationOutputs memory outputs = _outputs();
        outputs.l2BlockNumber = L2_BLOCK_NUMBER + 1;

        assertFalse(verifier.verify(_rootId(), _proof(outputs)));
    }

    function test_Verify_FalseForIntermediateRootsHashMismatch() public view {
        AggregationOutputs memory outputs = _outputs();
        bytes memory proof = _proof(
            DOMAIN_HASH, PARENT_REF, keccak256("wrong-intermediate"), L1_ORIGIN_NUMBER, outputs, SP1_PROOF_BYTES
        );

        assertFalse(verifier.verify(_rootId(), proof));
    }

    function test_Verify_FalseForL1HeadMismatch() public view {
        AggregationOutputs memory outputs = _outputs();
        outputs.l1Head = keccak256("wrong-l1-head");

        assertFalse(verifier.verify(_rootId(), _proof(outputs)));
    }

    function test_Verify_FalseForL1OriginNumberMismatch() public view {
        AggregationOutputs memory outputs = _outputs();
        bytes memory proof =
            _proof(DOMAIN_HASH, PARENT_REF, INTERMEDIATE_ROOTS_HASH, L1_ORIGIN_NUMBER + 1, outputs, SP1_PROOF_BYTES);

        assertFalse(verifier.verify(_rootId(), proof));
    }
}
