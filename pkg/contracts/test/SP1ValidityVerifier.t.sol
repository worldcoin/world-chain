// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";

import {ISP1Verifier} from "@sp1-contracts/src/ISP1Verifier.sol";
import {AggregationOutputs, SP1ValidityVerifier} from "../src/proofs/SP1ValidityVerifier.sol";

/// Stub SP1 verifier: accepts unless told to reject. Exercises the verifier's
/// field-binding logic without a real Groth16 proof (the mock-free path is the
/// devnet e2e test).
contract StubSP1Verifier is ISP1Verifier {
    bool public reject;

    function setReject(bool reject_) external {
        reject = reject_;
    }

    function verifyProof(bytes32, bytes calldata, bytes calldata) external view {
        require(!reject, "stub: invalid proof");
    }
}

/// Stub game exposing the proposal getters the verifier reads via msg.sender.
contract StubGame {
    bytes32 public rootClaim;
    uint256 public l2BlockNumber;
    bytes32 public l1OriginHash;

    constructor(bytes32 rootClaim_, uint256 l2BlockNumber_, bytes32 l1OriginHash_) {
        rootClaim = rootClaim_;
        l2BlockNumber = l2BlockNumber_;
        l1OriginHash = l1OriginHash_;
    }

    function callVerify(SP1ValidityVerifier verifier, bytes calldata proof) external view returns (bool) {
        return verifier.verify(bytes32(0), proof);
    }
}

contract SP1ValidityVerifierTest is Test {
    StubSP1Verifier internal sp1;
    SP1ValidityVerifier internal verifier;

    bytes32 internal constant AGG_VKEY = bytes32(uint256(0xA66));
    bytes32 internal constant ROLLUP_CONFIG_HASH = keccak256("rollup-config");
    bytes32 internal constant RANGE_VKEY = keccak256("range-vkey");

    bytes32 internal constant ROOT_CLAIM = keccak256("post-root");
    uint64 internal constant L2_BLOCK = 1200;
    bytes32 internal constant L1_ORIGIN = keccak256("l1-origin");

    function setUp() public {
        sp1 = new StubSP1Verifier();
        verifier = new SP1ValidityVerifier(sp1, AGG_VKEY, ROLLUP_CONFIG_HASH, RANGE_VKEY);
    }

    function _outputs() internal pure returns (AggregationOutputs memory) {
        return AggregationOutputs({
            l1Head: L1_ORIGIN,
            l2PreRoot: keccak256("pre-root"),
            l2PostRoot: ROOT_CLAIM,
            l2BlockNumber: L2_BLOCK,
            rollupConfigHash: ROLLUP_CONFIG_HASH,
            multiBlockVKey: RANGE_VKEY,
            proverAddress: address(0xBEEF)
        });
    }

    function _proof(AggregationOutputs memory outputs) internal pure returns (bytes memory) {
        return abi.encode(abi.encode(outputs), hex"deadbeef");
    }

    function _game() internal returns (StubGame) {
        return new StubGame(ROOT_CLAIM, L2_BLOCK, L1_ORIGIN);
    }

    function testVerifiesWhenOutputsMatchGame() public {
        StubGame game = _game();
        assertTrue(game.callVerify(verifier, _proof(_outputs())));
    }

    function testRevertsWhenSp1ProofInvalid() public {
        sp1.setReject(true);
        StubGame game = _game();
        vm.expectRevert(bytes("stub: invalid proof"));
        game.callVerify(verifier, _proof(_outputs()));
    }

    function testRevertsOnRootClaimMismatch() public {
        AggregationOutputs memory outputs = _outputs();
        outputs.l2PostRoot = keccak256("wrong-root");
        StubGame game = _game();
        vm.expectRevert(
            abi.encodeWithSelector(SP1ValidityVerifier.RootClaimMismatch.selector, ROOT_CLAIM, outputs.l2PostRoot)
        );
        game.callVerify(verifier, _proof(outputs));
    }

    function testRevertsOnBlockNumberMismatch() public {
        AggregationOutputs memory outputs = _outputs();
        outputs.l2BlockNumber = L2_BLOCK + 1;
        StubGame game = _game();
        vm.expectRevert(
            abi.encodeWithSelector(SP1ValidityVerifier.BlockNumberMismatch.selector, L2_BLOCK, outputs.l2BlockNumber)
        );
        game.callVerify(verifier, _proof(outputs));
    }

    function testRevertsOnL1HeadMismatch() public {
        AggregationOutputs memory outputs = _outputs();
        outputs.l1Head = keccak256("wrong-l1");
        StubGame game = _game();
        vm.expectRevert(abi.encodeWithSelector(SP1ValidityVerifier.L1HeadMismatch.selector, L1_ORIGIN, outputs.l1Head));
        game.callVerify(verifier, _proof(outputs));
    }

    function testRevertsOnRollupConfigHashMismatch() public {
        AggregationOutputs memory outputs = _outputs();
        outputs.rollupConfigHash = keccak256("wrong-config");
        StubGame game = _game();
        vm.expectRevert(
            abi.encodeWithSelector(
                SP1ValidityVerifier.RollupConfigHashMismatch.selector, ROLLUP_CONFIG_HASH, outputs.rollupConfigHash
            )
        );
        game.callVerify(verifier, _proof(outputs));
    }

    function testRevertsOnRangeVKeyMismatch() public {
        AggregationOutputs memory outputs = _outputs();
        outputs.multiBlockVKey = keccak256("wrong-range-vkey");
        StubGame game = _game();
        vm.expectRevert(
            abi.encodeWithSelector(SP1ValidityVerifier.RangeVKeyMismatch.selector, RANGE_VKEY, outputs.multiBlockVKey)
        );
        game.callVerify(verifier, _proof(outputs));
    }
}
