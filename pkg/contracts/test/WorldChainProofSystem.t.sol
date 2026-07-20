// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {Vm} from "forge-std/Vm.sol";

import {WorldChainAnchorStateRegistry} from "../src/proofs/WorldChainAnchorStateRegistry.sol";
import {WorldChainProofLib} from "../src/proofs/WorldChainProofLib.sol";
import {WorldChainProofSystemFactory} from "../src/proofs/WorldChainProofSystemFactory.sol";
import {WorldChainProofSystemGame} from "../src/proofs/WorldChainProofSystemGame.sol";
import {IWorldChainAnchorStateRegistry} from "../src/proofs/interfaces/IWorldChainAnchorStateRegistry.sol";
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
    address internal keeper = address(0xD00D);

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
        vm.deal(keeper, 100 ether);

        anchor = new WorldChainAnchorStateRegistry(bytes32(uint256(1)), 0);
        staking = new MockStakingRegistry();
        staking.setStaked(challenger, true);
        staking.setStaked(secondChallenger, true);

        validityVerifier = new MockRootIdVerifier(false);
        teeVerifier = new MockRootIdVerifier(false);
        councilVerifier = new MockRootIdVerifier(false);

        factory = _newFactory(anchor, WorldChainProofLib.PROOF_THRESHOLD);
        anchor.initializeFactory(address(factory));
    }

    function testUnchallengedRootFinalizesAfterChallengePeriod() public {
        (WorldChainProofSystemGame game,) = _propose(10);

        vm.warp(block.timestamp + CHALLENGE_PERIOD);
        game.resolve();

        assertEq(uint8(game.state()), uint8(WorldChainProofLib.RootState.FINALIZED));
        assertEq(game.claimable(proposer), PROPOSER_BOND);
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
        assertEq(game.challenger(), challenger);
        assertEq(game.postedChallengerBond(), CHALLENGER_BOND);
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

    function testProofThresholdDoesNotFinalizeUntilSettlement() public {
        (WorldChainProofSystemGame game, bytes32 rootId) = _proposeAndChallenge(10);

        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(rootId));

        // VALIDITY_PROOF (bit 0) and TEE_ATTESTATION (bit 1) produce the bitmap 0b011.
        vm.expectEmit(true, false, false, true);
        emit WorldChainProofSystemGame.ProofThresholdReached(rootId, 3);
        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.TEE_ATTESTATION), abi.encode(rootId));

        assertEq(game.proofCount(), 2);
        assertEq(uint8(game.state()), uint8(WorldChainProofLib.RootState.CHALLENGED));

        vm.recordLogs();
        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.SECURITY_COUNCIL), abi.encode(rootId));
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(logs.length, 1, "threshold event must only be emitted once");

        game.resolve();

        assertEq(uint8(game.state()), uint8(WorldChainProofLib.RootState.FINALIZED));
        assertEq(game.claimable(proposer), PROPOSER_BOND + CHALLENGER_BOND);
        assertEq(game.claimable(challenger), 0);
    }

    function testFinalizationCreditsSurplusEthToProposer() public {
        (WorldChainProofSystemGame game,) = _propose(10);
        uint256 surplus = 0.25 ether;
        vm.prank(keeper);
        (bool ok,) = address(game).call{value: surplus}("");
        assertTrue(ok);

        vm.warp(block.timestamp + CHALLENGE_PERIOD);
        game.resolve();

        _assertWithdraws(game, proposer, PROPOSER_BOND + surplus);
        assertEq(address(game).balance, 0);
    }

    function testThresholdOneRequiresExplicitSettlementAfterSingleLane() public {
        WorldChainAnchorStateRegistry thresholdAnchor = new WorldChainAnchorStateRegistry(bytes32(uint256(1)), 0);
        WorldChainProofSystemFactory thresholdOne = _newFactory(thresholdAnchor, 1);
        thresholdAnchor.initializeFactory(address(thresholdOne));

        vm.prank(proposer);
        (address gameAddress, bytes32 rootId) =
            thresholdOne.propose{value: PROPOSER_BOND}(address(thresholdAnchor), keccak256("threshold-one-root"), 10);
        WorldChainProofSystemGame game = WorldChainProofSystemGame(payable(gameAddress));
        assertEq(game.PROOF_THRESHOLD(), 1);

        _challenge(game, challenger);
        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(rootId));

        assertEq(game.proofCount(), 1);
        assertEq(uint8(game.state()), uint8(WorldChainProofLib.RootState.CHALLENGED));

        game.resolve();

        assertEq(uint8(game.state()), uint8(WorldChainProofLib.RootState.FINALIZED));
    }

    function testFactoryRejectsOutOfRangeThreshold() public {
        WorldChainAnchorStateRegistry thresholdAnchor = new WorldChainAnchorStateRegistry(bytes32(uint256(1)), 0);

        vm.expectRevert(WorldChainProofSystemFactory.InvalidActivationParameters.selector);
        _newFactory(thresholdAnchor, 0);

        vm.expectRevert(WorldChainProofSystemFactory.InvalidActivationParameters.selector);
        _newFactory(thresholdAnchor, WorldChainProofLib.PROOF_LANE_COUNT + 1);
    }

    function _newFactory(WorldChainAnchorStateRegistry registry, uint8 threshold)
        internal
        returns (WorldChainProofSystemFactory)
    {
        return new WorldChainProofSystemFactory(
            WorldChainProofLib.Domain({
                chainId: 4801,
                proofSystemVersion: 1,
                rollupConfigHash: keccak256("world-chain-devnet-rollup-config"),
                blockInterval: 10
            }),
            IWorldChainAnchorStateRegistry(address(registry)),
            CHALLENGE_PERIOD,
            PROOF_PERIOD,
            PROPOSER_BOND,
            CHALLENGER_BOND,
            threshold,
            validityVerifier,
            teeVerifier,
            councilVerifier,
            staking
        );
    }

    function testChallengedRootInvalidatesAfterProofDeadlineWithInsufficientLanes() public {
        (WorldChainProofSystemGame game, bytes32 rootId) = _proposeAndChallenge(10);
        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(rootId));

        vm.warp(block.timestamp + PROOF_PERIOD);

        (bool resolvable, WorldChainProofLib.RootState outcome, WorldChainProofLib.InvalidationReason reason) =
            game.resolutionStatus();
        assertTrue(resolvable);
        assertEq(uint8(outcome), uint8(WorldChainProofLib.RootState.INVALIDATED));
        assertEq(uint8(reason), uint8(WorldChainProofLib.InvalidationReason.PROOF_TIMEOUT));

        game.resolve();

        assertEq(uint8(game.state()), uint8(WorldChainProofLib.RootState.INVALIDATED));
        assertEq(uint8(game.invalidationReason()), uint8(WorldChainProofLib.InvalidationReason.PROOF_TIMEOUT));
    }

    function testResolutionStatusReportsWhenUnchallengedGameBecomesReady() public {
        (WorldChainProofSystemGame game,) = _propose(10);

        (bool resolvable, WorldChainProofLib.RootState outcome, WorldChainProofLib.InvalidationReason reason) =
            game.resolutionStatus();
        assertFalse(resolvable);
        assertEq(uint8(outcome), uint8(WorldChainProofLib.RootState.PROPOSED));
        assertEq(uint8(reason), uint8(WorldChainProofLib.InvalidationReason.NONE));

        vm.warp(block.timestamp + CHALLENGE_PERIOD);
        (resolvable, outcome, reason) = game.resolutionStatus();
        assertTrue(resolvable);
        assertEq(uint8(outcome), uint8(WorldChainProofLib.RootState.FINALIZED));
        assertEq(uint8(reason), uint8(WorldChainProofLib.InvalidationReason.NONE));
    }

    function testThresholdReadyChildWaitsForParentResolution() public {
        (WorldChainProofSystemGame parent,) = _propose(10);
        (WorldChainProofSystemGame child, bytes32 childRootId) = _proposeChild(parent, keccak256("child-root"));
        _challenge(child, challenger);
        child.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(childRootId));
        child.submitProofLane(uint8(WorldChainProofLib.ProofLane.TEE_ATTESTATION), abi.encode(childRootId));

        (bool resolvable, WorldChainProofLib.RootState outcome,) = child.resolutionStatus();
        assertFalse(resolvable);
        assertEq(uint8(outcome), uint8(WorldChainProofLib.RootState.CHALLENGED));

        vm.expectRevert(
            abi.encodeWithSelector(
                WorldChainProofSystemGame.ParentGameNotResolved.selector,
                address(parent),
                WorldChainProofLib.RootState.PROPOSED
            )
        );
        child.resolve();

        vm.warp(block.timestamp + CHALLENGE_PERIOD);
        parent.resolve();
        child.resolve();

        assertEq(uint8(parent.state()), uint8(WorldChainProofLib.RootState.FINALIZED));
        assertEq(uint8(child.state()), uint8(WorldChainProofLib.RootState.FINALIZED));
    }

    function testInvalidParentPropagatesBeforeChildProofTimeout() public {
        (WorldChainProofSystemGame parent,) = _proposeAndChallenge(10);
        (WorldChainProofSystemGame child,) = _proposeChild(parent, keccak256("inherited-child"));
        _challenge(child, secondChallenger);

        vm.warp(block.timestamp + PROOF_PERIOD);
        parent.resolve();

        uint256 proposerBalance = proposer.balance;
        uint256 childChallengerBalance = secondChallenger.balance;
        child.resolve();

        assertEq(uint8(child.state()), uint8(WorldChainProofLib.RootState.INVALIDATED));
        assertEq(uint8(child.invalidationReason()), uint8(WorldChainProofLib.InvalidationReason.INVALID_PARENT));
        assertEq(proposer.balance, proposerBalance);
        assertEq(secondChallenger.balance, childChallengerBalance);
        _assertWithdraws(child, proposer, PROPOSER_BOND);
        _assertWithdraws(child, secondChallenger, CHALLENGER_BOND);
    }

    function testBlacklistInvalidationRefundsBondAndSurplus() public {
        (WorldChainProofSystemGame parent,) = _propose(10);
        (WorldChainProofSystemGame game,) = _proposeChild(parent, keccak256("blacklisted-child"));
        _challenge(game, challenger);
        anchor.setGameBlacklisted(address(game), true);
        uint256 surplus = 0.25 ether;
        vm.prank(keeper);
        (bool ok,) = address(game).call{value: surplus}("");
        assertTrue(ok);

        uint256 proposerBalance = proposer.balance;
        uint256 challengerBalance = challenger.balance;
        game.resolve();

        assertEq(uint8(game.state()), uint8(WorldChainProofLib.RootState.INVALIDATED));
        assertEq(uint8(game.invalidationReason()), uint8(WorldChainProofLib.InvalidationReason.BLACKLISTED));
        assertEq(proposer.balance, proposerBalance);
        assertEq(challenger.balance, challengerBalance);
        _assertWithdraws(game, proposer, PROPOSER_BOND + surplus);
        _assertWithdraws(game, challenger, CHALLENGER_BOND);
        assertEq(address(game).balance, 0);
    }

    function testBlacklistedParentInvalidatesChild() public {
        (WorldChainProofSystemGame parent,) = _propose(10);
        (WorldChainProofSystemGame child,) = _proposeChild(parent, keccak256("blacklisted-parent-child"));
        anchor.setGameBlacklisted(address(parent), true);

        child.resolve();

        assertEq(uint8(child.state()), uint8(WorldChainProofLib.RootState.INVALIDATED));
        assertEq(uint8(child.invalidationReason()), uint8(WorldChainProofLib.InvalidationReason.INVALID_PARENT));
    }

    function testResolveRevertsWhileGameIsNotReadyAndAfterResolution() public {
        (WorldChainProofSystemGame game,) = _propose(10);

        vm.expectRevert(WorldChainProofSystemGame.NotReady.selector);
        game.resolve();

        vm.warp(block.timestamp + CHALLENGE_PERIOD);
        game.resolve();

        vm.expectRevert(
            abi.encodeWithSelector(
                WorldChainProofSystemGame.AlreadyResolved.selector, WorldChainProofLib.RootState.FINALIZED
            )
        );
        game.resolve();
    }

    function testDirectProofTimeoutRewardsChallenger() public {
        (WorldChainProofSystemGame game,) = _proposeAndChallenge(10);
        vm.warp(block.timestamp + PROOF_PERIOD);

        uint256 proposerBalance = proposer.balance;
        uint256 challengerBalance = challenger.balance;
        game.resolve();

        assertEq(proposer.balance, proposerBalance);
        assertEq(challenger.balance, challengerBalance);
        assertEq(game.postedChallengerBond(), 0);
        _assertWithdraws(game, challenger, CHALLENGER_BOND + PROPOSER_BOND);
        assertEq(game.claimable(proposer), 0);
    }

    function testDirectProofTimeoutCreditsSurplusEthToChallenger() public {
        (WorldChainProofSystemGame game,) = _proposeAndChallenge(10);
        uint256 surplus = 0.25 ether;
        vm.deal(address(game), address(game).balance + surplus);

        vm.warp(block.timestamp + PROOF_PERIOD);
        game.resolve();

        assertEq(game.postedChallengerBond(), 0);
        _assertWithdraws(game, challenger, CHALLENGER_BOND + PROPOSER_BOND + surplus);
        assertEq(game.claimable(proposer), 0);
        assertEq(address(game).balance, 0);
    }

    function testPermissionlessWithdrawPaysRecipientNotCaller() public {
        (WorldChainProofSystemGame game,) = _propose(10);

        vm.warp(block.timestamp + CHALLENGE_PERIOD);
        game.resolve();

        uint256 proposerBalance = proposer.balance;
        uint256 keeperBalance = keeper.balance;
        vm.prank(keeper);
        game.withdraw(payable(proposer));

        assertEq(proposer.balance, proposerBalance + PROPOSER_BOND);
        assertEq(game.claimable(proposer), 0);
        assertLe(keeper.balance, keeperBalance);
    }

    function testResolveDoesNotCallRevertingRecipient() public {
        RevertingReceiver rejecting = new RevertingReceiver();
        address rejectingProposer = address(rejecting);
        vm.deal(rejectingProposer, 100 ether);
        (WorldChainProofSystemGame game,) = _proposeFrom(rejectingProposer, 10);

        vm.warp(block.timestamp + CHALLENGE_PERIOD);
        game.resolve();

        assertEq(uint8(game.state()), uint8(WorldChainProofLib.RootState.FINALIZED));
        assertEq(game.claimable(rejectingProposer), PROPOSER_BOND);
        vm.expectRevert(
            abi.encodeWithSelector(WorldChainProofSystemGame.TransferFailed.selector, rejectingProposer, PROPOSER_BOND)
        );
        game.withdraw(payable(rejectingProposer));
        assertEq(game.claimable(rejectingProposer), PROPOSER_BOND);
    }

    function testLaneSubmissionAtProofDeadlineReverts() public {
        (WorldChainProofSystemGame game, bytes32 rootId) = _proposeAndChallenge(10);

        vm.warp(block.timestamp + PROOF_PERIOD);
        vm.expectRevert();
        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(rootId));
    }

    function testSecondChallengeReverts() public {
        (WorldChainProofSystemGame game,) = _proposeAndChallenge(10);
        uint64 firstDeadline = game.proofDeadline();

        vm.warp(block.timestamp + 1 hours);
        vm.prank(secondChallenger);
        vm.expectRevert(abi.encodeWithSelector(WorldChainProofSystemGame.DuplicateChallenge.selector, challenger));
        game.challenge{value: CHALLENGER_BOND}();

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

    function testFactoryIndexesGamesByProposalKeyWithoutL1Origin() public {
        bytes32 rootClaim = keccak256("root");
        bytes32 proposalKey = factory.computeProposalKey(address(anchor), rootClaim, 10);

        vm.prank(proposer);
        (address game, bytes32 rootId) = factory.propose{value: PROPOSER_BOND}(address(anchor), rootClaim, 10);

        assertEq(factory.games(proposalKey), game);
        assertEq(WorldChainProofSystemGame(payable(game)).rootId(), rootId);

        vm.roll(block.number + 1);
        vm.prank(proposer);
        vm.expectRevert(
            abi.encodeWithSelector(WorldChainProofSystemFactory.GameAlreadyExists.selector, proposalKey, game)
        );
        factory.propose{value: PROPOSER_BOND}(address(anchor), rootClaim, 10);
    }

    function testFactoryAllowsReplacementAttemptAfterInvalidation() public {
        bytes32 rootClaim = keccak256("retry-root");
        bytes32 proposalKey = factory.computeProposalKey(address(anchor), rootClaim, 10);

        vm.prank(proposer);
        (address firstAddress, bytes32 firstRootId) =
            factory.propose{value: PROPOSER_BOND}(address(anchor), rootClaim, 10);
        WorldChainProofSystemGame first = WorldChainProofSystemGame(payable(firstAddress));

        _challenge(first, challenger);
        vm.warp(block.timestamp + PROOF_PERIOD);
        first.resolve();
        vm.roll(block.number + 1);

        vm.prank(proposer);
        (address replacementAddress, bytes32 replacementRootId) =
            factory.propose{value: PROPOSER_BOND}(address(anchor), rootClaim, 10);
        WorldChainProofSystemGame replacement = WorldChainProofSystemGame(payable(replacementAddress));

        assertNotEq(replacementAddress, firstAddress);
        assertNotEq(replacementRootId, firstRootId);
        assertEq(replacement.attempt(), 1);
        assertEq(factory.games(proposalKey), replacementAddress);
        assertTrue(factory.isFactoryGame(firstAddress));
        assertTrue(factory.isFactoryGame(replacementAddress));
        assertEq(factory.gameCount(), 2);
        assertEq(factory.gameAt(0), firstAddress);
        assertEq(factory.gameAt(1), replacementAddress);
    }

    function testFactoryRequiresInheritedInvalidationToRebaseOnReplacementParent() public {
        (WorldChainProofSystemGame parent,) = _proposeAndChallenge(10);
        bytes32 childRootClaim = keccak256("retry-inherited-child");
        (WorldChainProofSystemGame child,) = _proposeChild(parent, childRootClaim);

        vm.warp(block.timestamp + PROOF_PERIOD);
        parent.resolve();
        child.resolve();

        bytes32 childProposalKey = factory.computeProposalKey(address(parent), childRootClaim, 20);
        vm.prank(proposer);
        vm.expectRevert(
            abi.encodeWithSelector(
                WorldChainProofSystemFactory.GameNotRetryable.selector,
                childProposalKey,
                address(child),
                WorldChainProofLib.InvalidationReason.INVALID_PARENT
            )
        );
        factory.propose{value: PROPOSER_BOND}(address(parent), childRootClaim, 20);

        vm.roll(block.number + 1);
        vm.prank(proposer);
        (address replacementParentAddress,) =
            factory.propose{value: PROPOSER_BOND}(address(anchor), parent.rootClaim(), 10);

        vm.prank(proposer);
        (address rebasedChildAddress,) =
            factory.propose{value: PROPOSER_BOND}(replacementParentAddress, childRootClaim, 20);
        WorldChainProofSystemGame rebasedChild = WorldChainProofSystemGame(payable(rebasedChildAddress));

        assertEq(rebasedChild.parentRef(), replacementParentAddress);
        assertEq(rebasedChild.attempt(), 0);
        assertNotEq(factory.computeProposalKey(replacementParentAddress, childRootClaim, 20), childProposalKey);
    }

    function testFactoryRequiresRegistryInitializationBeforePropose() public {
        WorldChainAnchorStateRegistry uninitializedAnchor = new WorldChainAnchorStateRegistry(bytes32(uint256(1)), 0);
        WorldChainProofSystemFactory uninitializedFactory =
            _newFactory(uninitializedAnchor, WorldChainProofLib.PROOF_THRESHOLD);

        vm.prank(proposer);
        vm.expectRevert(
            abi.encodeWithSelector(
                WorldChainProofSystemFactory.RegistryFactoryMismatch.selector, address(uninitializedFactory), address(0)
            )
        );
        uninitializedFactory.propose{value: PROPOSER_BOND}(
            address(uninitializedAnchor), keccak256("uninitialized-root"), 10
        );
    }

    function testRegistryFactoryCanOnlyBeInitializedOnce() public {
        vm.expectRevert(
            abi.encodeWithSelector(WorldChainAnchorStateRegistry.FactoryAlreadyInitialized.selector, address(factory))
        );
        anchor.initializeFactory(address(0xFACADE));
    }

    function testFactoryTracksCreatedGamesByIndexAndMembership() public {
        assertEq(factory.gameCount(), 0);

        (WorldChainProofSystemGame game,) = _propose(10);

        assertEq(factory.gameCount(), 1);
        assertEq(factory.gameAt(0), address(game));
        assertTrue(factory.isFactoryGame(address(game)));
        assertEq(game.factory(), address(factory));
        assertEq(game.anchorStateRegistry(), address(anchor));
        assertEq(game.attempt(), 0);
        assertEq(game.startingRootClaim(), anchor.currentRootClaim());
        assertEq(game.startingL2BlockNumber(), 0);
    }

    function testFactoryAllowsRegisteredGameParentAtNextInterval() public {
        (WorldChainProofSystemGame parent,) = _propose(10);

        vm.prank(proposer);
        (address childAddress,) = factory.propose{value: PROPOSER_BOND}(address(parent), keccak256("child-root"), 20);
        WorldChainProofSystemGame child = WorldChainProofSystemGame(payable(childAddress));

        assertEq(child.parentRef(), address(parent));
        assertEq(child.startingRootClaim(), parent.rootClaim());
        assertEq(child.startingL2BlockNumber(), parent.l2BlockNumber());
    }

    function testFactoryRejectsUnexpectedBlockInterval() public {
        vm.prank(proposer);
        vm.expectRevert(
            abi.encodeWithSelector(WorldChainProofSystemFactory.InvalidL2BlockNumber.selector, uint256(10), uint256(20))
        );
        factory.propose{value: PROPOSER_BOND}(address(anchor), keccak256("wrong-block"), 20);
    }

    function testFactoryRejectsUnregisteredParent() public {
        address unknownParent = address(0xFACE);

        vm.prank(proposer);
        vm.expectRevert(abi.encodeWithSelector(WorldChainProofSystemFactory.InvalidParent.selector, unknownParent));
        factory.propose{value: PROPOSER_BOND}(unknownParent, keccak256("unknown-parent"), 10);
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
        invalidated.resolve();
        vm.expectRevert();
        anchor.setAnchorState(address(invalidated));

        (WorldChainProofSystemGame finalized,) = _finalizedGame(10);
        anchor.setPaused(true);
        vm.expectRevert();
        anchor.setAnchorState(address(finalized));
        anchor.setPaused(false);

        anchor.setAnchorState(address(finalized));
        vm.expectRevert();
        anchor.setAnchorState(address(finalized));
    }

    function testAnchorRejectsFinalizedGameFromDifferentFactory() public {
        WorldChainAnchorStateRegistry otherAnchor = new WorldChainAnchorStateRegistry(bytes32(uint256(1)), 0);
        WorldChainProofSystemFactory otherFactory = _newFactory(otherAnchor, WorldChainProofLib.PROOF_THRESHOLD);
        otherAnchor.initializeFactory(address(otherFactory));

        vm.prank(proposer);
        (address gameAddress, bytes32 rootId) =
            otherFactory.propose{value: PROPOSER_BOND}(address(otherAnchor), keccak256("other-root"), 10);
        WorldChainProofSystemGame game = WorldChainProofSystemGame(payable(gameAddress));
        _challenge(game, challenger);
        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(rootId));
        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.TEE_ATTESTATION), abi.encode(rootId));
        game.resolve();

        vm.expectRevert(
            abi.encodeWithSelector(
                WorldChainAnchorStateRegistry.InvalidGameFactory.selector, address(factory), address(otherFactory)
            )
        );
        anchor.setAnchorState(address(game));
    }

    function _propose(uint256 l2BlockNumber) internal returns (WorldChainProofSystemGame game, bytes32 rootId) {
        return _proposeFrom(proposer, l2BlockNumber);
    }

    function _proposeFrom(address account, uint256 l2BlockNumber)
        internal
        returns (WorldChainProofSystemGame game, bytes32 rootId)
    {
        proposalSalt++;
        vm.prank(account);
        (address gameAddress, bytes32 id) = factory.propose{value: PROPOSER_BOND}(
            address(anchor), keccak256(abi.encode("root", l2BlockNumber, proposalSalt)), l2BlockNumber
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

    function _proposeChild(WorldChainProofSystemGame parent, bytes32 rootClaim)
        internal
        returns (WorldChainProofSystemGame game, bytes32 rootId)
    {
        uint256 l2BlockNumber = parent.l2BlockNumber() + 10;
        vm.prank(proposer);
        (address gameAddress, bytes32 id) =
            factory.propose{value: PROPOSER_BOND}(address(parent), rootClaim, l2BlockNumber);
        return (WorldChainProofSystemGame(payable(gameAddress)), id);
    }

    function _finalizedGame(uint256 l2BlockNumber) internal returns (WorldChainProofSystemGame game, bytes32 rootId) {
        (game, rootId) = _proposeAndChallenge(l2BlockNumber);
        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(rootId));
        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.TEE_ATTESTATION), abi.encode(rootId));
        game.resolve();
    }

    function _challenge(WorldChainProofSystemGame game, address account) internal {
        vm.prank(account);
        game.challenge{value: CHALLENGER_BOND}();
    }

    function _assertWithdraws(WorldChainProofSystemGame game, address account, uint256 amount) internal {
        assertEq(game.claimable(account), amount);
        uint256 balance = account.balance;
        game.withdraw(payable(account));
        assertEq(account.balance, balance + amount);
        assertEq(game.claimable(account), 0);
    }
}

contract RevertingReceiver {
    receive() external payable {
        revert("reject eth");
    }
}
