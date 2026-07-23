// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {Vm} from "forge-std/Vm.sol";

import {WorldChainAnchorStateRegistry} from "../src/proofs/WorldChainAnchorStateRegistry.sol";
import {
    Claim,
    GameId,
    GameStatus,
    GameType,
    Hash,
    Proposal,
    Timestamp,
    GameTypes
} from "../src/proofs/DisputeTypes.sol";
import {WorldChainProofLib} from "../src/proofs/WorldChainProofLib.sol";
import {WorldChainProofSystemFactory} from "../src/proofs/WorldChainProofSystemFactory.sol";
import {WorldChainProofSystemGame} from "../src/proofs/WorldChainProofSystemGame.sol";
import {IDisputeGame} from "../src/proofs/interfaces/IDisputeGame.sol";
import {
    IOptimismPortal2AnchorStateRegistry,
    IOptimismPortal2DisputeGameFactory
} from "../src/proofs/interfaces/IOptimismPortal2.sol";
import {IWorldChainAnchorStateRegistry} from "../src/proofs/interfaces/IWorldChainAnchorStateRegistry.sol";
import {IWorldChainProofSystemFactory} from "../src/proofs/interfaces/IWorldChainProofSystemFactory.sol";
import {MockRootIdVerifier} from "../src/proofs/mocks/MockRootIdVerifier.sol";
import {MockStakingRegistry} from "../src/proofs/mocks/MockStakingRegistry.sol";

contract WorldChainProofSystemTest is Test {
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);

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
        bytes32 proposalKey = _proposalKey(address(anchor), rootClaim, 10);

        vm.prank(proposer);
        (address game, bytes32 rootId) = factory.propose{value: PROPOSER_BOND}(address(anchor), rootClaim, 10);

        assertEq(_registeredGame(address(anchor), rootClaim, 10), game);
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
        assertEq(_registeredGame(address(anchor), rootClaim, 10), replacementAddress);
        assertEq(factory.gameCount(), 2);
        (,, IDisputeGame firstIndexedGame) = factory.gameAtIndex(0);
        (,, IDisputeGame replacementIndexedGame) = factory.gameAtIndex(1);
        assertEq(address(firstIndexedGame), firstAddress);
        assertEq(address(replacementIndexedGame), replacementAddress);
    }

    function testFactoryRequiresInheritedInvalidationToRebaseOnReplacementParent() public {
        (WorldChainProofSystemGame parent,) = _proposeAndChallenge(10);
        bytes32 childRootClaim = keccak256("retry-inherited-child");
        (WorldChainProofSystemGame child,) = _proposeChild(parent, childRootClaim);

        vm.warp(block.timestamp + PROOF_PERIOD);
        parent.resolve();
        child.resolve();

        bytes32 childProposalKey = _proposalKey(address(parent), childRootClaim, 20);
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
        assertNotEq(_proposalKey(replacementParentAddress, childRootClaim, 20), childProposalKey);
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

    function testRegistryOwnershipTransferRejectsZeroAndEmits() public {
        vm.expectRevert(abi.encodeWithSelector(WorldChainAnchorStateRegistry.InvalidOwner.selector, address(0)));
        anchor.transferOwnership(address(0));

        vm.expectEmit(true, true, false, true);
        emit OwnershipTransferred(address(this), keeper);
        anchor.transferOwnership(keeper);

        assertEq(anchor.owner(), keeper);
        vm.expectRevert(WorldChainAnchorStateRegistry.NotOwner.selector);
        anchor.setPaused(true);
        vm.prank(keeper);
        anchor.setPaused(true);
        assertTrue(anchor.paused());
    }

    function testFactoryTracksCreatedGamesByIndex() public {
        assertEq(factory.gameCount(), 0);

        (WorldChainProofSystemGame game,) = _propose(10);

        assertEq(factory.gameCount(), 1);
        (,, IDisputeGame indexedGame) = factory.gameAtIndex(0);
        assertEq(address(indexedGame), address(game));
        assertTrue(anchor.isGameRegistered(address(game)));
        assertEq(game.factory(), address(factory));
        assertEq(game.anchorStateRegistry(), address(anchor));
        assertEq(game.attempt(), 0);
        (bytes32 anchorRoot,) = _anchorRoot();
        assertEq(game.startingRootClaim(), anchorRoot);
        assertEq(game.startingL2BlockNumber(), 0);
    }

    function testFactoryAllowsOnlyOneLiveAnchorRegistryParent() public {
        (WorldChainProofSystemGame first,) = _propose(10);

        vm.prank(proposer);
        vm.expectRevert(WorldChainProofSystemFactory.AnchorRegistryParentAlreadyUsed.selector);
        factory.propose{value: PROPOSER_BOND}(address(anchor), keccak256("alternative-initial-root"), 10);

        vm.warp(block.timestamp + CHALLENGE_PERIOD);
        first.resolve();
        vm.warp(block.timestamp + 1);
        first.closeGame();
        bytes32 childRoot = keccak256("child-root");

        vm.prank(proposer);
        vm.expectRevert(WorldChainProofSystemFactory.AnchorRegistryParentAlreadyUsed.selector);
        factory.propose{value: PROPOSER_BOND}(address(anchor), childRoot, 20);

        (WorldChainProofSystemGame child,) = _proposeChild(first, childRoot);
        assertEq(child.parentRef(), address(first));
    }

    function testFactoryAllowsCorrectedStartingProposalAfterTimeout() public {
        (WorldChainProofSystemGame first,) = _proposeAndChallenge(10);
        vm.warp(block.timestamp + PROOF_PERIOD);
        first.resolve();

        vm.prank(proposer);
        (address correctedAddress,) =
            factory.propose{value: PROPOSER_BOND}(address(anchor), keccak256("corrected-root"), 10);
        WorldChainProofSystemGame corrected = WorldChainProofSystemGame(payable(correctedAddress));

        assertEq(corrected.parentRef(), address(anchor));
        assertEq(corrected.attempt(), 0);
    }

    function testFactoryAllowsStartingProposalAfterBlacklistedGameInvalidates() public {
        (WorldChainProofSystemGame first,) = _propose(10);
        anchor.setGameBlacklisted(address(first), true);
        first.resolve();

        vm.prank(proposer);
        (address replacementAddress,) =
            factory.propose{value: PROPOSER_BOND}(address(anchor), keccak256("replacement-root"), 10);

        assertEq(WorldChainProofSystemGame(payable(replacementAddress)).parentRef(), address(anchor));
    }

    function testFactoryExposesOpGameIndexAndGameDataLookup() public {
        (WorldChainProofSystemGame game,) = _propose(10);

        (GameType indexedType, Timestamp createdAt, IDisputeGame indexedGame) = factory.gameAtIndex(0);
        assertEq(GameType.unwrap(indexedType), GameType.unwrap(GameTypes.WIP_1006));
        assertEq(Timestamp.unwrap(createdAt), game.createdAt());
        assertEq(address(indexedGame), address(game));

        (GameType gameType, Claim rootClaim, bytes memory extraData) = IDisputeGame(address(game)).gameData();
        assertEq(GameType.unwrap(gameType), GameType.unwrap(GameTypes.WIP_1006));
        assertEq(Claim.unwrap(rootClaim), game.rootClaim());

        (uint256 l2BlockNumber, address parentRef) = abi.decode(extraData, (uint256, address));
        assertEq(l2BlockNumber, game.l2SequenceNumber());
        assertEq(parentRef, game.parentRef());

        (IDisputeGame registeredGame, Timestamp registeredAt) = factory.games(gameType, rootClaim, extraData);
        assertEq(address(registeredGame), address(game));
        assertEq(Timestamp.unwrap(registeredAt), game.createdAt());
    }

    function testContractsImplementPinnedOptimismPortal2Boundary() public {
        (WorldChainProofSystemGame game,) = _propose(10);
        IOptimismPortal2AnchorStateRegistry portalRegistry = IOptimismPortal2AnchorStateRegistry(address(anchor));
        IOptimismPortal2DisputeGameFactory portalFactory = IOptimismPortal2DisputeGameFactory(address(factory));

        assertEq(portalRegistry.disputeGameFactory(), address(factory));
        assertEq(portalRegistry.disputeGameFinalityDelaySeconds(), 0);
        assertEq(portalRegistry.retirementTimestamp(), 0);
        assertFalse(portalRegistry.disputeGameBlacklist(address(game)));
        assertTrue(portalRegistry.isGameProper(address(game)));
        assertTrue(portalRegistry.isGameRespected(address(game)));
        assertFalse(portalRegistry.isGameClaimValid(address(game)));

        (GameType gameType, Timestamp createdAt, IDisputeGame indexedGame) = portalFactory.gameAtIndex(0);
        assertEq(GameType.unwrap(gameType), GameType.unwrap(GameTypes.WIP_1006));
        assertEq(Timestamp.unwrap(createdAt), game.createdAt());
        assertEq(address(indexedGame), address(game));
    }

    function testFactoryFindLatestGamesMatchesOpWithdrawalDiscovery() public {
        (WorldChainProofSystemGame first,) = _propose(10);
        (WorldChainProofSystemGame second,) = _proposeChild(first, keccak256("second-root"));

        IWorldChainProofSystemFactory.GameSearchResult[] memory games =
            factory.findLatestGames(GameTypes.WIP_1006, 1, 2);

        assertEq(games.length, 2);
        assertEq(games[0].index, 1);
        assertEq(Claim.unwrap(games[0].rootClaim), second.rootClaim());
        assertEq(Timestamp.unwrap(games[0].timestamp), second.createdAt());
        assertEq(GameId.unwrap(games[0].metadata), _gameId(address(second), second.createdAt()));
        (uint256 l2SequenceNumber, address parentRef) = abi.decode(games[0].extraData, (uint256, address));
        assertEq(l2SequenceNumber, second.l2SequenceNumber());
        assertEq(parentRef, second.parentRef());

        assertEq(games[1].index, 0);
        assertEq(Claim.unwrap(games[1].rootClaim), first.rootClaim());
        assertEq(GameId.unwrap(games[1].metadata), _gameId(address(first), first.createdAt()));
    }

    function testFactoryFindLatestGamesReturnsEmptyForUnsupportedSearch() public {
        _propose(10);

        assertEq(factory.findLatestGames(GameType.wrap(1), 0, 1).length, 0);
        assertEq(factory.findLatestGames(GameTypes.WIP_1006, 1, 1).length, 0);
        assertEq(factory.findLatestGames(GameTypes.WIP_1006, 0, 0).length, 0);
    }

    function testGameExposesOpDisputeGameFacade() public {
        (WorldChainProofSystemGame game, bytes32 rootId) = _proposeAndChallenge(10);
        IDisputeGame opGame = IDisputeGame(address(game));

        assertEq(uint8(opGame.status()), uint8(GameStatus.IN_PROGRESS));
        assertEq(Timestamp.unwrap(opGame.resolvedAt()), 0);
        assertEq(opGame.gameCreator(), proposer);
        assertEq(opGame.rootClaim(), game.rootClaim());
        assertEq(Claim.unwrap(opGame.rootClaimByChainId(4801)), game.rootClaim());
        assertEq(opGame.l2SequenceNumber(), game.l2SequenceNumber());
        assertEq(GameType.unwrap(opGame.gameType()), GameType.unwrap(GameTypes.WIP_1006));
        assertEq(opGame.wasRespectedGameTypeWhenCreated(), true);

        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(rootId));
        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.TEE_ATTESTATION), abi.encode(rootId));
        game.resolve();

        assertEq(uint8(opGame.status()), uint8(GameStatus.DEFENDER_WINS));
        assertEq(Timestamp.unwrap(opGame.resolvedAt()), Timestamp.unwrap(game.resolvedAt()));
    }

    function testInvalidatedGameExposesChallengerWinAndInvalidClaim() public {
        (WorldChainProofSystemGame game,) = _proposeAndChallenge(10);

        vm.warp(block.timestamp + PROOF_PERIOD);
        game.resolve();
        vm.warp(block.timestamp + 1);

        assertEq(uint8(IDisputeGame(address(game)).status()), uint8(GameStatus.CHALLENGER_WINS));
        assertFalse(anchor.isGameClaimValid(address(game)));
    }

    function testAnchorPredicatesMatchPortalExpectations() public {
        (WorldChainProofSystemGame game, bytes32 rootId) = _proposeAndChallenge(10);

        assertTrue(anchor.isGameRegistered(address(game)));
        assertTrue(anchor.isGameProper(address(game)));
        assertTrue(anchor.isGameRespected(address(game)));
        assertFalse(anchor.isGameClaimValid(address(game)));

        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(rootId));
        game.submitProofLane(uint8(WorldChainProofLib.ProofLane.TEE_ATTESTATION), abi.encode(rootId));
        game.resolve();

        assertFalse(anchor.isGameFinalized(address(game)));
        assertFalse(anchor.isGameClaimValid(address(game)));

        vm.warp(block.timestamp + 1);
        assertTrue(anchor.isGameFinalized(address(game)));
        assertTrue(anchor.isGameClaimValid(address(game)));

        anchor.setPaused(true);
        assertFalse(anchor.isGameProper(address(game)));
        assertFalse(anchor.isGameClaimValid(address(game)));
    }

    function testRespectedGameTypeIsSnapshottedAtGameCreation() public {
        (WorldChainProofSystemGame respectedGame,) = _propose(10);
        anchor.setRespectedGameType(GameType.wrap(1));

        assertTrue(respectedGame.wasRespectedGameTypeWhenCreated());
        assertTrue(anchor.isGameRespected(address(respectedGame)));

        (WorldChainProofSystemGame unrespectedGame,) = _proposeChild(respectedGame, keccak256("unrespected-root"));
        address unrespectedGameAddress = address(unrespectedGame);

        assertFalse(WorldChainProofSystemGame(payable(unrespectedGameAddress)).wasRespectedGameTypeWhenCreated());
        assertFalse(anchor.isGameRespected(unrespectedGameAddress));
    }

    function testOpRegistrationFollowsReplacementAttempt() public {
        bytes32 rootClaim = keccak256("op-replacement-root");

        vm.prank(proposer);
        (address firstAddress, bytes32 firstRootId) =
            factory.propose{value: PROPOSER_BOND}(address(anchor), rootClaim, 10);
        WorldChainProofSystemGame first = WorldChainProofSystemGame(payable(firstAddress));

        _challenge(first, challenger);
        vm.warp(block.timestamp + PROOF_PERIOD);
        first.resolve();
        vm.warp(block.timestamp + 1);
        assertFalse(anchor.isGameClaimValid(firstAddress));

        vm.roll(block.number + 1);
        vm.prank(proposer);
        (address replacementAddress,) = factory.propose{value: PROPOSER_BOND}(address(anchor), rootClaim, 10);

        assertEq(first.rootId(), firstRootId);
        assertFalse(anchor.isGameRegistered(firstAddress));
        assertTrue(anchor.isGameRegistered(replacementAddress));
    }

    function testFactoryAllowsRegisteredGameParentAtNextInterval() public {
        (WorldChainProofSystemGame parent,) = _propose(10);

        vm.prank(proposer);
        (address childAddress,) = factory.propose{value: PROPOSER_BOND}(address(parent), keccak256("child-root"), 20);
        WorldChainProofSystemGame child = WorldChainProofSystemGame(payable(childAddress));

        assertEq(child.parentRef(), address(parent));
        assertEq(child.startingRootClaim(), parent.rootClaim());
        assertEq(child.startingL2BlockNumber(), parent.l2SequenceNumber());
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

        (, uint256 anchorBlock) = _anchorRoot();
        assertEq(anchorBlock, 10);
        assertEq(anchor.anchorGame(), address(game));
    }

    function testGameCanCloseThroughAnchorRegistry() public {
        (WorldChainProofSystemGame game,) = _finalizedGame(10);

        game.closeGame();

        (bytes32 anchorRoot, uint256 anchorBlock) = _anchorRoot();
        assertEq(anchorRoot, game.rootClaim());
        assertEq(anchorBlock, game.l2SequenceNumber());
        assertEq(anchor.anchorGame(), address(game));
    }

    function testStartingAnchorRootDoesNotMoveWhenAnchorAdvances() public {
        Proposal memory startingAnchor = anchor.getStartingAnchorRoot();
        (WorldChainProofSystemGame game,) = _finalizedGame(10);

        game.closeGame();

        Proposal memory unchangedStartingAnchor = anchor.getStartingAnchorRoot();
        assertEq(Hash.unwrap(unchangedStartingAnchor.root), Hash.unwrap(startingAnchor.root));
        assertEq(unchangedStartingAnchor.l2SequenceNumber, startingAnchor.l2SequenceNumber);
        (bytes32 currentRoot, uint256 currentBlock) = _anchorRoot();
        assertEq(currentRoot, game.rootClaim());
        assertEq(currentBlock, game.l2SequenceNumber());
    }

    function testGameClosePropagatesAnchorRegistryRejection() public {
        (WorldChainProofSystemGame game,) = _propose(10);

        vm.expectRevert(abi.encodeWithSelector(WorldChainAnchorStateRegistry.GameNotFinalized.selector, address(game)));
        game.closeGame();
    }

    function testFinalizedGameIsEligibleToCloseAfterAsrTimestampCheck() public {
        (WorldChainProofSystemGame game,) = _finalizedGame(10);

        assertTrue(anchor.isGameFinalized(address(game)));
        assertTrue(anchor.isGameClaimValid(address(game)));

        anchor.setPaused(true);
        assertFalse(anchor.isGameClaimValid(address(game)));
        anchor.setPaused(false);

        game.closeGame();
        assertEq(anchor.anchorGame(), address(game));
    }

    function testAnchorCanJumpToHighestFinalizedDescendant() public {
        (WorldChainProofSystemGame first,) = _propose(10);
        (WorldChainProofSystemGame second,) = _proposeChild(first, keccak256("second-root"));
        (WorldChainProofSystemGame third,) = _proposeChild(second, keccak256("third-root"));

        vm.warp(block.timestamp + CHALLENGE_PERIOD);
        first.resolve();
        second.resolve();
        third.resolve();
        vm.warp(block.timestamp + 1);

        anchor.setAnchorState(address(third));

        (bytes32 anchorRoot, uint256 anchorBlock) = _anchorRoot();
        assertEq(anchorRoot, third.rootClaim());
        assertEq(anchorBlock, third.l2SequenceNumber());
        assertEq(anchor.anchorGame(), address(third));
    }

    function testAnchorAcceptsNewerFinalizedParallelBranch() public {
        (WorldChainProofSystemGame first,) = _propose(10);
        (WorldChainProofSystemGame acceptedBranch,) = _proposeChild(first, keccak256("accepted-root"));
        (WorldChainProofSystemGame parallelParent,) = _proposeChild(first, keccak256("parallel-parent-root"));
        (WorldChainProofSystemGame parallelChild,) = _proposeChild(parallelParent, keccak256("parallel-child-root"));

        vm.warp(block.timestamp + CHALLENGE_PERIOD);
        first.resolve();
        acceptedBranch.resolve();
        parallelParent.resolve();
        parallelChild.resolve();
        vm.warp(block.timestamp + 1);

        anchor.setAnchorState(address(acceptedBranch));
        anchor.setAnchorState(address(parallelChild));

        assertEq(anchor.anchorGame(), address(parallelChild));
        (bytes32 anchorRoot, uint256 anchorBlock) = _anchorRoot();
        assertEq(anchorRoot, parallelChild.rootClaim());
        assertEq(anchorBlock, parallelChild.l2SequenceNumber());
    }

    function testFinalizedAncestorCannotBeBlacklisted() public {
        (WorldChainProofSystemGame first,) = _propose(10);
        (WorldChainProofSystemGame second,) = _proposeChild(first, keccak256("second-root"));
        (WorldChainProofSystemGame third,) = _proposeChild(second, keccak256("third-root"));

        vm.warp(block.timestamp + CHALLENGE_PERIOD);
        first.resolve();
        second.resolve();
        third.resolve();

        vm.expectRevert(
            abi.encodeWithSelector(
                WorldChainAnchorStateRegistry.FinalizedGameCannotBeBlacklisted.selector, address(second)
            )
        );
        anchor.setGameBlacklisted(address(second), true);
        assertFalse(anchor.blacklistedGames(address(second)));

        vm.warp(block.timestamp + 1);
        third.closeGame();

        assertEq(anchor.anchorGame(), address(third));
    }

    function testAnchorAcceptsAlternativeLineageFromSameCheckpoint() public {
        bytes32 sharedRootClaim = keccak256("shared-root");
        (WorldChainProofSystemGame initialGame,) = _propose(10);
        (WorldChainProofSystemGame first,) = _proposeChild(initialGame, keccak256("first-root"));
        (WorldChainProofSystemGame acceptedCheckpoint,) = _proposeChild(first, sharedRootClaim);
        (WorldChainProofSystemGame alternativeFirst,) = _proposeChild(initialGame, keccak256("alternative-first-root"));
        (WorldChainProofSystemGame equivalentCheckpoint,) = _proposeChild(alternativeFirst, sharedRootClaim);

        vm.warp(block.timestamp + CHALLENGE_PERIOD);
        initialGame.resolve();
        first.resolve();
        acceptedCheckpoint.resolve();
        alternativeFirst.resolve();
        equivalentCheckpoint.resolve();
        vm.warp(block.timestamp + 1);
        anchor.setAnchorState(address(acceptedCheckpoint));

        (WorldChainProofSystemGame alternativeSuccessor,) =
            _proposeChild(equivalentCheckpoint, keccak256("alternative-successor-root"));
        vm.warp(block.timestamp + CHALLENGE_PERIOD);
        alternativeSuccessor.resolve();
        vm.warp(block.timestamp + 1);
        anchor.setAnchorState(address(alternativeSuccessor));

        assertEq(anchor.anchorGame(), address(alternativeSuccessor));
        (bytes32 anchorRoot, uint256 anchorBlock) = _anchorRoot();
        assertEq(anchorRoot, alternativeSuccessor.rootClaim());
        assertEq(anchorBlock, alternativeSuccessor.l2SequenceNumber());
    }

    function testAnchorRejectsNonFinalizedInvalidatedPausedAndNonMonotonicRoots() public {
        (WorldChainProofSystemGame nonFinalized,) = _propose(10);
        vm.expectRevert();
        anchor.setAnchorState(address(nonFinalized));

        vm.warp(block.timestamp + CHALLENGE_PERIOD);
        nonFinalized.resolve();

        (WorldChainProofSystemGame invalidated, bytes32 invalidatedRootId) =
            _proposeChild(nonFinalized, keccak256("invalidated-root"));
        _challenge(invalidated, challenger);
        invalidated.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(invalidatedRootId));
        vm.warp(block.timestamp + PROOF_PERIOD);
        invalidated.resolve();
        vm.expectRevert();
        anchor.setAnchorState(address(invalidated));

        (WorldChainProofSystemGame finalized, bytes32 finalizedRootId) =
            _proposeChild(nonFinalized, keccak256("finalized-root"));
        _challenge(finalized, challenger);
        finalized.submitProofLane(uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF), abi.encode(finalizedRootId));
        finalized.submitProofLane(uint8(WorldChainProofLib.ProofLane.TEE_ATTESTATION), abi.encode(finalizedRootId));
        finalized.resolve();
        vm.warp(block.timestamp + 1);
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
        uint256 l2BlockNumber = parent.l2SequenceNumber() + 10;
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
        vm.warp(block.timestamp + 1);
    }

    function _challenge(WorldChainProofSystemGame game, address account) internal {
        vm.prank(account);
        game.challenge{value: CHALLENGER_BOND}();
    }

    function _registeredGame(address parentRef, bytes32 rootClaim, uint256 l2BlockNumber)
        internal
        view
        returns (address)
    {
        (IDisputeGame game,) =
            factory.games(GameTypes.WIP_1006, Claim.wrap(rootClaim), abi.encode(l2BlockNumber, parentRef));
        return address(game);
    }

    function _proposalKey(address parentRef, bytes32 rootClaim, uint256 l2BlockNumber) internal view returns (bytes32) {
        return WorldChainProofLib.proposalKey(factory.domainHash(), parentRef, rootClaim, l2BlockNumber);
    }

    function _gameId(address game, uint64 timestamp) internal pure returns (bytes32) {
        return bytes32(
            (uint256(GameType.unwrap(GameTypes.WIP_1006)) << 224) | (uint256(timestamp) << 160) | uint256(uint160(game))
        );
    }

    function _anchorRoot() internal view returns (bytes32 root, uint256 l2SequenceNumber) {
        Hash anchorRoot;
        (anchorRoot, l2SequenceNumber) = anchor.getAnchorRoot();
        root = Hash.unwrap(anchorRoot);
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
