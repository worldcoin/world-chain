// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";

import {ISP1Verifier} from "@sp1-contracts/src/ISP1Verifier.sol";
import {AggregationPublicValues, SP1ValidityVerifier} from "../../src/proofs/sp1/SP1ValidityVerifier.sol";
import {WorldChainProofLib} from "../../src/proofs/WorldChainProofLib.sol";
import {MockProofSystemFactory, MockProofSystemGame} from "../mocks/MockProofSystemGame.sol";

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

contract StubParentGame {
    bytes32 public rootClaim;

    constructor(bytes32 rootClaim_) {
        rootClaim = rootClaim_;
    }

    function setRootClaim(bytes32 rootClaim_) external {
        rootClaim = rootClaim_;
    }
}

contract StubAnchorStateRegistry {
    bytes32 public currentRootClaim;

    constructor(bytes32 currentRootClaim_) {
        currentRootClaim = currentRootClaim_;
    }

    function setCurrentRootClaim(bytes32 currentRootClaim_) external {
        currentRootClaim = currentRootClaim_;
    }
}

contract SP1ValidityVerifierTest is Test {
    StubSP1Verifier internal sp1;
    StubAnchorStateRegistry internal anchor;
    StubParentGame internal parent;
    SP1ValidityVerifier internal verifier;
    MockProofSystemGame internal game;
    MockProofSystemFactory internal proofSystemFactory;
    bytes32 internal domainHash;

    bytes32 internal constant AGGREGATION_VKEY = bytes32(uint256(0xA66));
    bytes32 internal constant ROLLUP_CONFIG_HASH = keccak256("world-chain-rollup-config");
    bytes32 internal constant RANGE_VKEY_COMMITMENT = keccak256("range-vkey");

    bytes32 internal constant L1_ORIGIN_HASH = keccak256("l1-origin");
    uint256 internal constant L1_ORIGIN_NUMBER = 9_001;

    bytes32 internal constant L2_PRE_ROOT = keccak256("l2-pre-root");
    bytes32 internal constant L2_POST_ROOT = keccak256("l2-post-root");
    uint64 internal constant L2_PRE_BLOCK_NUMBER = 123_455;
    uint64 internal constant L2_BLOCK_NUMBER = 123_456;

    bytes internal constant SP1_PROOF_BYTES = hex"4388a21cdeadbeef";

    function setUp() public {
        sp1 = new StubSP1Verifier();
        anchor = new StubAnchorStateRegistry(L2_PRE_ROOT);
        parent = new StubParentGame(L2_PRE_ROOT);
        WorldChainProofLib.Domain memory domain = WorldChainProofLib.Domain({
            chainId: 480,
            proofSystemVersion: 1,
            rollupConfigHash: ROLLUP_CONFIG_HASH,
            blockInterval: L2_BLOCK_NUMBER - L2_PRE_BLOCK_NUMBER
        });
        proofSystemFactory = new MockProofSystemFactory(domain);
        domainHash = WorldChainProofLib.domainHash(domain);
        verifier = new SP1ValidityVerifier(ISP1Verifier(address(sp1)), AGGREGATION_VKEY, RANGE_VKEY_COMMITMENT);
        game = new MockProofSystemGame();
        _setGameContext(address(parent));
    }

    /*//////////////////////////////////////////////////////////////
                                HELPERS
    //////////////////////////////////////////////////////////////*/

    function _publicValuesStruct() internal pure returns (AggregationPublicValues memory) {
        return AggregationPublicValues({
            transitionPublicValues: WorldChainProofLib.TransitionPublicValues({
                l1Head: L1_ORIGIN_HASH,
                l2PreRoot: L2_PRE_ROOT,
                l2PreBlockNumber: L2_PRE_BLOCK_NUMBER,
                l2PostRoot: L2_POST_ROOT,
                l2PostBlockNumber: L2_BLOCK_NUMBER,
                rollupConfigHash: ROLLUP_CONFIG_HASH
            }),
            multiBlockVKey: RANGE_VKEY_COMMITMENT
        });
    }

    function _publicValues(AggregationPublicValues memory values) internal pure returns (bytes memory) {
        return abi.encode(values);
    }

    function _proof(AggregationPublicValues memory values) internal view returns (bytes memory) {
        return _proof(domainHash, address(parent), L1_ORIGIN_NUMBER, values, SP1_PROOF_BYTES);
    }

    function _proof(
        bytes32 domainHash_,
        address parentRef,
        uint256 l1OriginNumber,
        AggregationPublicValues memory values,
        bytes memory proofBytes
    ) internal pure returns (bytes memory) {
        return abi.encode(domainHash_, parentRef, l1OriginNumber, _publicValues(values), proofBytes);
    }

    function _rootId() internal view returns (bytes32) {
        return _rootId(address(parent));
    }

    function _rootId(address parentRef) internal view returns (bytes32) {
        return WorldChainProofLib.rootId(
            domainHash, parentRef, L2_POST_ROOT, uint256(L2_BLOCK_NUMBER), L1_ORIGIN_HASH, L1_ORIGIN_NUMBER
        );
    }

    function _expectSp1Call(AggregationPublicValues memory values) internal {
        sp1.setExpectation(AGGREGATION_VKEY, _publicValues(values), SP1_PROOF_BYTES);
    }

    function _setGameContext(address parentRef) internal {
        game.setContext(
            MockProofSystemGame.Context({
                factory: address(proofSystemFactory),
                rootId: _rootId(parentRef),
                anchorStateRegistry: address(anchor),
                domainHash: domainHash,
                parentRef: parentRef,
                startingRootClaim: L2_PRE_ROOT,
                startingL2BlockNumber: L2_PRE_BLOCK_NUMBER,
                rootClaim: L2_POST_ROOT,
                l2BlockNumber: L2_BLOCK_NUMBER,
                l1OriginHash: L1_ORIGIN_HASH,
                l1OriginNumber: L1_ORIGIN_NUMBER
            })
        );
    }

    function _verify(bytes32 rootId, bytes memory proof) internal view returns (bool) {
        return game.verify(address(verifier), rootId, proof);
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
                              HAPPY PATH
    //////////////////////////////////////////////////////////////*/

    function test_Verify_HappyPath_WithParentGame() public {
        AggregationPublicValues memory outputs = _publicValuesStruct();
        _expectSp1Call(outputs);

        assertTrue(_verify(_rootId(), _proof(outputs)));
    }

    function test_Verify_HappyPath_WithAnchorRegistryParent() public {
        _setGameContext(address(anchor));
        AggregationPublicValues memory outputs = _publicValuesStruct();
        _expectSp1Call(outputs);

        bytes memory proof = _proof(domainHash, address(anchor), L1_ORIGIN_NUMBER, outputs, SP1_PROOF_BYTES);

        assertTrue(_verify(_rootId(address(anchor)), proof));
    }

    function test_Verify_UsesGameSnapshotAfterAnchorAdvances() public {
        _setGameContext(address(anchor));
        anchor.setCurrentRootClaim(keccak256("new-anchor-root"));

        AggregationPublicValues memory outputs = _publicValuesStruct();
        _expectSp1Call(outputs);
        bytes memory proof = _proof(domainHash, address(anchor), L1_ORIGIN_NUMBER, outputs, SP1_PROOF_BYTES);

        assertTrue(_verify(_rootId(address(anchor)), proof));
    }

    /*//////////////////////////////////////////////////////////////
                            DECODE FAILURES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseForMalformedOuterProof() public view {
        assertFalse(_verify(_rootId(), hex"deadbeef"));
    }

    function test_Verify_FalseForMalformedPublicValues() public view {
        bytes memory proof = abi.encode(domainHash, address(parent), L1_ORIGIN_NUMBER, hex"deadbeef", SP1_PROOF_BYTES);

        assertFalse(_verify(_rootId(), proof));
    }

    /*//////////////////////////////////////////////////////////////
                            SP1 GATEWAY FAILURES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseWhenSP1ProofInvalid() public {
        AggregationPublicValues memory outputs = _publicValuesStruct();
        _expectSp1Call(outputs);
        sp1.setReject(true);

        assertFalse(_verify(_rootId(), _proof(outputs)));
    }

    function test_Verify_FalseWhenUnexpectedProofBytesForwarded() public {
        AggregationPublicValues memory outputs = _publicValuesStruct();
        sp1.setExpectation(AGGREGATION_VKEY, _publicValues(outputs), bytes("unexpected"));

        assertFalse(_verify(_rootId(), _proof(outputs)));
    }

    /*//////////////////////////////////////////////////////////////
                            PUBLIC VALUE GATES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseForRollupConfigHashMismatch() public view {
        AggregationPublicValues memory outputs = _publicValuesStruct();
        outputs.transitionPublicValues.rollupConfigHash = keccak256("wrong-rollup-config");

        assertFalse(_verify(_rootId(), _proof(outputs)));
    }

    function test_Verify_FalseForRangeVKeyMismatch() public view {
        AggregationPublicValues memory outputs = _publicValuesStruct();
        outputs.multiBlockVKey = keccak256("wrong-range-vkey");

        assertFalse(_verify(_rootId(), _proof(outputs)));
    }

    function test_Verify_FalseForParentGamePreRootMismatch() public view {
        AggregationPublicValues memory outputs = _publicValuesStruct();
        outputs.transitionPublicValues.l2PreRoot = keccak256("wrong-pre-root");

        assertFalse(_verify(_rootId(), _proof(outputs)));
    }

    function test_Verify_FalseForAnchorRegistryPreRootMismatch() public {
        _setGameContext(address(anchor));
        AggregationPublicValues memory outputs = _publicValuesStruct();
        outputs.transitionPublicValues.l2PreRoot = keccak256("wrong-pre-root");
        bytes memory proof = _proof(domainHash, address(anchor), L1_ORIGIN_NUMBER, outputs, SP1_PROOF_BYTES);

        assertFalse(_verify(_rootId(address(anchor)), proof));
    }

    function test_Verify_FalseForPreBlockNumberMismatch() public view {
        AggregationPublicValues memory outputs = _publicValuesStruct();
        outputs.transitionPublicValues.l2PreBlockNumber = L2_PRE_BLOCK_NUMBER + 1;

        assertFalse(_verify(_rootId(), _proof(outputs)));
    }

    function test_Verify_FalseForUnreadableParentRef() public view {
        AggregationPublicValues memory outputs = _publicValuesStruct();
        address unreadableParentRef = address(0xBEEF);
        bytes memory proof = _proof(domainHash, unreadableParentRef, L1_ORIGIN_NUMBER, outputs, SP1_PROOF_BYTES);

        assertFalse(_verify(_rootId(unreadableParentRef), proof));
    }

    /*//////////////////////////////////////////////////////////////
                             ROOT ID GATES
    //////////////////////////////////////////////////////////////*/

    function test_Verify_FalseForWrongRootId() public {
        AggregationPublicValues memory outputs = _publicValuesStruct();
        _expectSp1Call(outputs);

        assertFalse(_verify(bytes32(uint256(0xdead)), _proof(outputs)));
    }

    function test_Verify_FalseForDomainHashMismatch() public view {
        AggregationPublicValues memory outputs = _publicValuesStruct();
        bytes memory proof =
            _proof(keccak256("wrong-domain"), address(parent), L1_ORIGIN_NUMBER, outputs, SP1_PROOF_BYTES);

        assertFalse(_verify(_rootId(), proof));
    }

    function test_Verify_FalseForParentRefMismatch() public view {
        AggregationPublicValues memory outputs = _publicValuesStruct();
        bytes memory proof = _proof(domainHash, address(0xBAD), L1_ORIGIN_NUMBER, outputs, SP1_PROOF_BYTES);

        assertFalse(_verify(_rootId(), proof));
    }

    function test_Verify_FalseForPostRootMismatch() public view {
        AggregationPublicValues memory outputs = _publicValuesStruct();
        outputs.transitionPublicValues.l2PostRoot = keccak256("wrong-post-root");

        assertFalse(_verify(_rootId(), _proof(outputs)));
    }

    function test_Verify_FalseForBlockNumberMismatch() public view {
        AggregationPublicValues memory outputs = _publicValuesStruct();
        outputs.transitionPublicValues.l2PostBlockNumber = L2_BLOCK_NUMBER + 1;

        assertFalse(_verify(_rootId(), _proof(outputs)));
    }

    function test_Verify_FalseForL1HeadMismatch() public view {
        AggregationPublicValues memory outputs = _publicValuesStruct();
        outputs.transitionPublicValues.l1Head = keccak256("wrong-l1-head");

        assertFalse(_verify(_rootId(), _proof(outputs)));
    }

    function test_Verify_FalseForL1OriginNumberMismatch() public view {
        AggregationPublicValues memory outputs = _publicValuesStruct();
        bytes memory proof = _proof(domainHash, address(parent), L1_ORIGIN_NUMBER + 1, outputs, SP1_PROOF_BYTES);

        assertFalse(_verify(_rootId(), proof));
    }
}
