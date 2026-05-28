// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";

import {WorldChainAnchorStateRegistry} from "../src/proofs/WorldChainAnchorStateRegistry.sol";
import {WorldChainProofLib} from "../src/proofs/WorldChainProofLib.sol";
import {WorldChainProofSystemFactory} from "../src/proofs/WorldChainProofSystemFactory.sol";
import {WorldChainProofSystemGame} from "../src/proofs/WorldChainProofSystemGame.sol";
import {MockRootIdVerifier} from "../src/proofs/mocks/MockRootIdVerifier.sol";
import {MockStakingRegistry} from "../src/proofs/mocks/MockStakingRegistry.sol";

contract WorldChainProofSystemTest is Test {
    uint64 internal constant CHALLENGE_PERIOD = 1 days;
    uint64 internal constant PROOF_PERIOD = 7 days;
    uint256 internal constant PROPOSER_BOND = 1 ether;
    uint256 internal constant CHALLENGER_BOND = 0.1 ether;

    address internal proposer = address(0xA11CE);
    address internal challenger = address(0xB0B);
    address internal secondChallenger = address(0xCAFE);

    WorldChainAnchorStateRegistry internal anchor;
    WorldChainProofSystemFactory internal factory;
    MockRootIdVerifier internal validityVerifier;
    MockRootIdVerifier internal teeVerifier;
    MockRootIdVerifier internal councilVerifier;
    MockStakingRegistry internal staking;
    uint256 internal proposalSalt;

    function setUp() public {
        vm.deal(proposer, 100 ether);
        vm.deal(challenger, 100 ether);
        vm.deal(secondChallenger, 100 ether);

        anchor = new WorldChainAnchorStateRegistry(bytes32(uint256(1)), 0);
        staking = new MockStakingRegistry();
        staking.setStaked(challenger, true);
        staking.setStaked(secondChallenger, true);

        validityVerifier = new MockRootIdVerifier(false);
        teeVerifier = new MockRootIdVerifier(false);
        councilVerifier = new MockRootIdVerifier(false);

        factory = new WorldChainProofSystemFactory(
            WorldChainProofLib.Domain({
                chainId: 4801,
                proofSystemVersion: 1,
                rollupConfigHash: keccak256("world-chain-devnet-rollup-config"),
                blockInterval: 10,
                intermediateBlockInterval: 5
            }),
            CHALLENGE_PERIOD,
            PROOF_PERIOD,
            PROPOSER_BOND,
            CHALLENGER_BOND,
            validityVerifier,
            teeVerifier,
            councilVerifier,
            staking
        );
    }

    function testUnchallengedRootFinalizesAfterChallengePeriod() public {
        (WorldChainProofSystemGame game,) = _propose(10);

        vm.warp(block.timestamp + CHALLENGE_PERIOD);
        game.finalize();

        assertEq(uint8(game.state()), uint8(WorldChainProofLib.RootState.FINALIZED));
    }

    function testUnstakedAccountCannotChallenge() public {
        (WorldChainProofSystemGame game,) = _propose(10);

        vm.deal(address(0xBAD), 1 ether);
        vm.prank(address(0xBAD));
        vm.expectRevert();
        game.challenge{value: CHALLENGER_BOND}();
    }

    function testStakedAccountCanChallengeWithoutProofBeforeDeadline() public {
        (WorldChainProofSystemGame game,) = _propose(10);

        _challenge(game, challenger);

        assertEq(uint8(game.state()), uint8(WorldChainProofLib.RootState.CHALLENGED));
        assertEq(game.proofDeadline(), block.timestamp + PROOF_PERIOD);
    }

    function testChallengeAtOrAfterDeadlineReverts() public {
        (WorldChainProofSystemGame game,) = _propose(10);

        vm.warp(block.timestamp + CHALLENGE_PERIOD);
        vm.prank(challenger);
        vm.expectRevert();
        game.challenge{value: CHALLENGER_BOND}();
    }

    function testOneSupportingLaneDoesNotFinalize() public {
        (WorldChainProofSystemGame game, bytes32 rootId) = _proposeAndChallenge(10);

        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(rootId));

        assertEq(game.proofCount(), 1);
        assertEq(uint8(game.state()), uint8(WorldChainProofLib.RootState.CHALLENGED));
    }

    function testProposerDefendsWithTwoDistinctLanes() public {
        (WorldChainProofSystemGame game, bytes32 rootId) = _proposeAndChallenge(10);

        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(rootId));
        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.TEE_ATTESTATION), abi.encode(rootId));

        assertEq(game.proofCount(), 2);
        assertEq(uint8(game.state()), uint8(WorldChainProofLib.RootState.FINALIZED));
    }

    function testChallengedRootInvalidatesAfterProofDeadlineWithInsufficientLanes() public {
        (WorldChainProofSystemGame game, bytes32 rootId) = _proposeAndChallenge(10);
        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(rootId));

        vm.warp(block.timestamp + PROOF_PERIOD);
        game.invalidate();

        assertEq(uint8(game.state()), uint8(WorldChainProofLib.RootState.INVALIDATED));
    }

    function testLaneSubmissionAtProofDeadlineReverts() public {
        (WorldChainProofSystemGame game, bytes32 rootId) = _proposeAndChallenge(10);

        vm.warp(block.timestamp + PROOF_PERIOD);
        vm.expectRevert();
        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(rootId));
    }

    function testSubsequentChallengeDoesNotExtendProofDeadline() public {
        (WorldChainProofSystemGame game,) = _proposeAndChallenge(10);
        uint64 firstDeadline = game.proofDeadline();

        vm.warp(block.timestamp + 1 hours);
        _challenge(game, secondChallenger);

        assertEq(game.proofDeadline(), firstDeadline);
    }

    function testDuplicateLaneDoesNotIncreaseThresholdCount() public {
        (WorldChainProofSystemGame game, bytes32 rootId) = _proposeAndChallenge(10);

        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(rootId));
        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(rootId));

        assertEq(game.proofCount(), 1);
        assertEq(uint8(game.state()), uint8(WorldChainProofLib.RootState.CHALLENGED));
    }

    function testProofLaneRejectsMaterialForDifferentRootId() public {
        (WorldChainProofSystemGame game,) = _proposeAndChallenge(10);

        vm.expectRevert();
        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(bytes32(uint256(0x1234))));
    }

    function testAnchorUpdatesOnlyAcceptFinalizedMonotonicRoots() public {
        (WorldChainProofSystemGame game,) = _finalizedGame(10);

        anchor.setAnchorState(address(game));

        assertEq(anchor.currentL2BlockNumber(), 10);
        assertEq(anchor.anchorGame(), address(game));
    }

    function testAnchorRejectsNonFinalizedInvalidatedPausedAndNonMonotonicRoots() public {
        (WorldChainProofSystemGame nonFinalized,) = _propose(10);
        vm.expectRevert();
        anchor.setAnchorState(address(nonFinalized));

        (WorldChainProofSystemGame invalidated, bytes32 rootId) = _proposeAndChallenge(10);
        invalidated.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(rootId));
        vm.warp(block.timestamp + PROOF_PERIOD);
        invalidated.invalidate();
        vm.expectRevert();
        anchor.setAnchorState(address(invalidated));

        (WorldChainProofSystemGame finalized,) = _finalizedGame(10);
        anchor.setPaused(true);
        vm.expectRevert();
        anchor.setAnchorState(address(finalized));
        anchor.setPaused(false);

        anchor.setAnchorState(address(finalized));
        (WorldChainProofSystemGame lower,) = _finalizedGame(5);
        vm.expectRevert();
        anchor.setAnchorState(address(lower));
    }

    function _propose(uint256 l2BlockNumber) internal returns (WorldChainProofSystemGame game, bytes32 rootId) {
        proposalSalt++;
        vm.prank(proposer);
        (address gameAddress, bytes32 id) = factory.propose{value: PROPOSER_BOND}(
            address(anchor),
            keccak256(abi.encode("root", l2BlockNumber, proposalSalt)),
            l2BlockNumber,
            keccak256(abi.encode("intermediate", l2BlockNumber, proposalSalt))
        );
        return (WorldChainProofSystemGame(payable(gameAddress)), id);
    }

    function _proposeAndChallenge(uint256 l2BlockNumber)
        internal
        returns (WorldChainProofSystemGame game, bytes32 rootId)
    {
        (game, rootId) = _propose(l2BlockNumber);
        _challenge(game, challenger);
    }

    function _finalizedGame(uint256 l2BlockNumber) internal returns (WorldChainProofSystemGame game, bytes32 rootId) {
        (game, rootId) = _proposeAndChallenge(l2BlockNumber);
        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(rootId));
        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.TEE_ATTESTATION), abi.encode(rootId));
    }

    function _challenge(WorldChainProofSystemGame game, address account) internal {
        vm.prank(account);
        game.challenge{value: CHALLENGER_BOND}();
    }
}
