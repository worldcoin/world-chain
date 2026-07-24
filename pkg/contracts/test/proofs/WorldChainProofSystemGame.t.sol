// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {OPStackFixtures} from "./OPStackFixtures.sol";
import {WorldChainProofSystemGame} from "../../src/proofs/WorldChainProofSystemGame.sol";
import {WorldChainProofLib} from "../../src/proofs/WorldChainProofLib.sol";

import {BondDistributionMode, Claim, GameStatus, GameType, Hash} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {
    AlreadyInitialized,
    BadExtraData,
    ClaimAlreadyChallenged,
    ClaimAlreadyResolved,
    GameAlreadyExists,
    GameNotFinalized,
    GameNotOver,
    GamePaused,
    IncorrectBondAmount,
    InvalidParentGame,
    ParentGameNotResolved,
    UnknownChainId
} from "@optimism-bedrock/src/dispute/lib/Errors.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";

contract WorldChainProofSystemGameTest is OPStackFixtures {
    /*//////////////////////////////////////////////////////////////
                        CREATION / DGF INTEGRATION
    //////////////////////////////////////////////////////////////*/

    function test_Create_CWIARoundTrip() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;

        assertEq(game.gameCreator(), proposer);
        assertEq(Claim.unwrap(game.rootClaim()), _rootClaimFor(target));
        assertEq(Hash.unwrap(game.l1Head()), blockhash(block.number - 1));
        assertEq(game.l2SequenceNumber(), target);
        assertEq(game.l2BlockNumber(), target);
        assertEq(game.proposalDomainHash(), game.domainHash());
        assertEq(game.attempt(), 0);
        assertEq(game.parentRef(), address(asr));
        assertEq(game.startingRootClaim(), STARTING_ANCHOR_ROOT);
        assertEq(game.startingL2BlockNumber(), STARTING_ANCHOR_BLOCK);
        assertEq(GameType.unwrap(game.gameType()), GameType.unwrap(WC_GAME_TYPE));
        assertTrue(game.wasRespectedGameTypeWhenCreated());
        assertEq(uint8(game.status()), uint8(GameStatus.IN_PROGRESS));
        assertEq(uint8(game.state()), uint8(WorldChainProofLib.RootState.PROPOSED));
        assertEq(game.l1OriginNumber(), block.number - 1);
        assertEq(
            game.rootId(),
            WorldChainProofLib.rootId(
                game.domainHash(),
                address(asr),
                _rootClaimFor(target),
                target,
                blockhash(block.number - 1),
                block.number - 1
            )
        );

        // The factory round-trips gameData back to this game (ASR registration predicate).
        (GameType gt, Claim rc, bytes memory ed) = game.gameData();
        (IDisputeGame registered,) = dgf.games(gt, rc, ed);
        assertEq(address(registered), address(game));
        assertTrue(asr.isGameRegistered(IDisputeGame(address(game))));

        // The proposer bond is custodied in DelayedWETH, not the game.
        assertEq(address(game).balance, 0);
        assertEq(weth.balanceOf(address(game)), PROPOSER_BOND);
    }

    function test_Create_EmitsWorldChainGameCreated() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        vm.expectEmit(false, false, false, false);
        emit WorldChainProofSystemGame.WorldChainGameCreated(
            bytes32(0), address(0), bytes32(0), 0, bytes32(0), 0, 0, address(0)
        );
        _propose(type(uint256).max, _rootClaimFor(target), target, 0);
    }

    function test_Create_DuplicateProposalReverts() public {
        _proposeAtAnchor();
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        bytes memory extra = _extraData(target, type(uint256).max, 0);
        Hash uuid = dgf.getGameUUID(WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), extra);

        vm.prank(proposer);
        vm.expectRevert(abi.encodeWithSelector(GameAlreadyExists.selector, uuid));
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), extra);
    }

    function test_Create_WrongBondReverts() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        vm.prank(proposer);
        vm.expectRevert(IncorrectBondAmount.selector);
        dgf.create{value: PROPOSER_BOND + 1}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), _extraData(target, type(uint256).max, 0)
        );
    }

    function test_Create_BadExtraDataLengthReverts() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;

        vm.prank(proposer);
        vm.expectRevert(BadExtraData.selector);
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), abi.encode(target, type(uint256).max)
        );

        vm.prank(proposer);
        vm.expectRevert(BadExtraData.selector);
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE,
            Claim.wrap(_rootClaimFor(target)),
            abi.encode(bytes32(0), target, address(asr), uint256(0), uint256(0))
        );
    }

    function test_Create_NonCanonicalParentEncodingReverts() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        uint256 malformedParent = uint256(uint160(address(asr))) | (uint256(1) << 160);
        bytes memory extraData =
            abi.encode(WorldChainProofLib.domainHash(_domain()), target, malformedParent, uint256(0));

        vm.prank(proposer);
        vm.expectRevert(BadExtraData.selector);
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), extraData);
    }

    function test_Create_DomainMismatchReverts() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        bytes32 wrongDomain = keccak256("wrong-domain");
        bytes memory extraData = abi.encode(wrongDomain, target, address(asr), uint256(0));
        vm.prank(proposer);
        vm.expectRevert(
            abi.encodeWithSelector(
                WorldChainProofSystemGame.InvalidDomainHash.selector, gameImpl.domainHash(), wrongDomain
            )
        );
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), extraData);
    }

    function test_Initialize_DirectImplementationCallReverts() public {
        // The bare implementation sees 4-byte calldata: no CWIA payload appended.
        vm.expectRevert(BadExtraData.selector);
        gameImpl.initialize();
    }

    function test_Initialize_ReinitializationReverts() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        // The CWIA proxy appends the immutable args to every call, so the length check passes
        // and the initialized flag rejects.
        vm.expectRevert(AlreadyInitialized.selector);
        game.initialize();
    }

    function test_Create_RevertsWhenPaused() public {
        systemConfig.setPaused(true);
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        vm.prank(proposer);
        vm.expectRevert(GamePaused.selector);
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), _extraData(target, type(uint256).max, 0)
        );
    }

    function test_Create_RejectsUnexpectedBlockInterval() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL + 1;
        vm.prank(proposer);
        vm.expectRevert(
            abi.encodeWithSelector(
                WorldChainProofSystemGame.InvalidL2BlockNumber.selector, STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL, target
            )
        );
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), _extraData(target, type(uint256).max, 0)
        );
    }

    function test_Create_UnregisteredParentReverts() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        bytes memory extraData = _extraDataForParent(target, makeAddr("unregistered-parent"), 0);
        vm.prank(proposer);
        vm.expectRevert(InvalidParentGame.selector);
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), extraData);
    }

    function test_Create_ParentRegisteredUnderDifferentGameTypeReverts() public {
        // Register a second implementation under game type 43 and create a game with it.
        GameType otherType = GameType.wrap(43);
        WorldChainProofSystemGame otherImpl = new WorldChainProofSystemGame(_gameConfig());
        dgf.setImplementation(otherType, IDisputeGame(address(otherImpl)), hex"");
        dgf.setInitBond(otherType, PROPOSER_BOND);
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        vm.prank(proposer);
        dgf.create{value: PROPOSER_BOND}(
            otherType, Claim.wrap(_rootClaimFor(target)), _extraData(target, type(uint256).max, 0)
        );

        // Chaining a WIP-1006 child onto a game registered under type 43 must fail.
        uint256 childTarget = target + BLOCK_INTERVAL;
        bytes memory childExtraData = _extraData(childTarget, 0, 0);
        vm.prank(proposer);
        vm.expectRevert(InvalidParentGame.selector);
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, Claim.wrap(_rootClaimFor(childTarget)), childExtraData);
    }

    function test_Create_InvalidatedParentReverts() public {
        WorldChainProofSystemGame parent = _proposeAtAnchor();
        _challenge(parent);
        vm.warp(parent.proofDeadline());
        parent.resolve();
        assertEq(uint8(parent.status()), uint8(GameStatus.CHALLENGER_WINS));

        uint256 childTarget = parent.l2SequenceNumber() + BLOCK_INTERVAL;
        bytes memory childExtraData = _extraData(childTarget, 0, 0);
        vm.prank(proposer);
        vm.expectRevert(InvalidParentGame.selector);
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, Claim.wrap(_rootClaimFor(childTarget)), childExtraData);
    }

    function test_Create_BlacklistedParentReverts() public {
        WorldChainProofSystemGame parent = _proposeAtAnchor();
        vm.prank(guardian);
        asr.blacklistDisputeGame(IDisputeGame(address(parent)));

        uint256 childTarget = parent.l2SequenceNumber() + BLOCK_INTERVAL;
        bytes memory childExtraData = _extraData(childTarget, 0, 0);
        vm.prank(proposer);
        vm.expectRevert(InvalidParentGame.selector);
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, Claim.wrap(_rootClaimFor(childTarget)), childExtraData);
    }

    function test_Create_ParentAtOrBelowAnchorRequiresSentinel() public {
        // Finalize the first game and advance the anchor to it.
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);
        game.closeGame();
        (, uint256 anchorBlock) = asr.getAnchorRoot();
        assertEq(anchorBlock, game.l2SequenceNumber());

        // Chaining onto the now-anchored game by index is rejected as stale...
        uint256 childTarget = game.l2SequenceNumber() + BLOCK_INTERVAL;
        bytes memory childExtraData = _extraData(childTarget, 0, 0);
        vm.prank(proposer);
        vm.expectRevert(InvalidParentGame.selector);
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, Claim.wrap(_rootClaimFor(childTarget)), childExtraData);

        // ...while the anchor sentinel path proposes the same transition.
        WorldChainProofSystemGame child = _proposeAtAnchor();
        assertEq(child.startingRootClaim(), Claim.unwrap(game.rootClaim()));
        assertEq(child.startingL2BlockNumber(), game.l2SequenceNumber());
    }

    /*//////////////////////////////////////////////////////////////
                          CONSTRUCTOR VALIDATION
    //////////////////////////////////////////////////////////////*/

    function test_Constructor_RejectsOutOfRangeThreshold() public {
        WorldChainProofSystemGame.GameConfig memory config = _gameConfig();
        config.proofThreshold = 0;
        vm.expectRevert(WorldChainProofSystemGame.InvalidActivationParameters.selector);
        new WorldChainProofSystemGame(config);

        config.proofThreshold = PROOF_LANE_COUNT_PLUS_ONE();
        vm.expectRevert(WorldChainProofSystemGame.InvalidActivationParameters.selector);
        new WorldChainProofSystemGame(config);
    }

    function PROOF_LANE_COUNT_PLUS_ONE() internal pure returns (uint8) {
        return WorldChainProofLib.PROOF_LANE_COUNT + 1;
    }

    function test_Constructor_RejectsZeroParameters() public {
        WorldChainProofSystemGame.GameConfig memory config = _gameConfig();
        config.challengePeriod = 0;
        vm.expectRevert(WorldChainProofSystemGame.InvalidActivationParameters.selector);
        new WorldChainProofSystemGame(config);

        config = _gameConfig();
        config.domain.blockInterval = 0;
        vm.expectRevert(WorldChainProofSystemGame.InvalidActivationParameters.selector);
        new WorldChainProofSystemGame(config);
    }

    function test_Constructor_RejectsProofPeriodAtOrBeforeChallengePeriod() public {
        WorldChainProofSystemGame.GameConfig memory config = _gameConfig();
        config.proofPeriod = config.challengePeriod;
        vm.expectRevert(WorldChainProofSystemGame.InvalidActivationParameters.selector);
        new WorldChainProofSystemGame(config);
    }

    function test_Constructor_RejectsDomainChainIdMismatch() public {
        WorldChainProofSystemGame.GameConfig memory config = _gameConfig();
        config.domain.chainId = CHAIN_ID + 1;
        vm.expectRevert(WorldChainProofSystemGame.InconsistentSystemConfiguration.selector);
        new WorldChainProofSystemGame(config);
    }

    /*//////////////////////////////////////////////////////////////
                          CHALLENGE WINDOW
    //////////////////////////////////////////////////////////////*/

    function test_UnchallengedGame_FinalizesAfterChallengeWindow() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();

        vm.expectEmit(true, false, false, false, address(game));
        emit WorldChainProofSystemGame.Resolved(GameStatus.DEFENDER_WINS);
        _resolveUnchallenged(game);

        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));
        assertEq(uint8(game.state()), uint8(WorldChainProofLib.RootState.FINALIZED));
        assertEq(game.finalizedAt(), uint64(block.timestamp));
        assertEq(game.credit(proposer), PROPOSER_BOND);
    }

    function test_Resolve_RevertsBeforeChallengeDeadline() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        vm.expectRevert(GameNotOver.selector);
        game.resolve();
    }

    function test_Resolve_RevertsWhenAlreadyResolved() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        vm.expectRevert(ClaimAlreadyResolved.selector);
        game.resolve();
    }

    function test_Challenge_RevertsForUnstaked() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        address unstaked = makeAddr("unstaked");
        vm.deal(unstaked, 1 ether);
        vm.prank(unstaked);
        vm.expectRevert(abi.encodeWithSelector(WorldChainProofSystemGame.UnstakedChallenger.selector, unstaked));
        game.challenge{value: CHALLENGER_BOND}();
    }

    function test_Challenge_SucceedsForStaked() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        uint64 proofDeadline = game.proofDeadline();
        _challenge(game);
        assertEq(game.challenger(), challengerAccount);
        assertEq(game.challengedAt(), uint64(block.timestamp));
        assertEq(uint8(game.state()), uint8(WorldChainProofLib.RootState.CHALLENGED));
        assertEq(game.proofDeadline(), proofDeadline);
        assertEq(weth.balanceOf(address(game)), PROPOSER_BOND + CHALLENGER_BOND);
    }

    function test_Challenge_DoesNotExtendCreationRelativeProofDeadline() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        uint64 proofDeadline = game.proofDeadline();
        vm.warp(game.challengeDeadline() - 1);

        _challenge(game);

        assertEq(game.proofDeadline(), proofDeadline);
        assertLt(game.proofDeadline() - game.challengedAt(), game.proofPeriod());
    }

    function test_Challenge_RevertsAtOrAfterDeadline() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        vm.warp(game.challengeDeadline());
        vm.prank(challengerAccount);
        vm.expectRevert(
            abi.encodeWithSelector(
                WorldChainProofSystemGame.ChallengePeriodElapsed.selector, block.timestamp, game.challengeDeadline()
            )
        );
        game.challenge{value: CHALLENGER_BOND}();
    }

    function test_Challenge_RevertsOnSecondChallenge() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _challenge(game);
        stakingRegistry.setStaked(address(this), true);
        vm.expectRevert(ClaimAlreadyChallenged.selector);
        game.challenge{value: CHALLENGER_BOND}();
    }

    function test_Challenge_RevertsForWrongBond() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        vm.prank(challengerAccount);
        vm.expectRevert(IncorrectBondAmount.selector);
        game.challenge{value: CHALLENGER_BOND - 1}();
    }

    /*//////////////////////////////////////////////////////////////
                        PROOF LANES / THRESHOLD
    //////////////////////////////////////////////////////////////*/

    function test_SubmitProofLane_RevertsWhenUnchallenged() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        bytes memory proof = abi.encodePacked(game.rootId());
        vm.expectRevert(ClaimAlreadyResolved.selector);
        game.submitProofLane(0, proof);
    }

    function test_SubmitProofLane_OneLaneInsufficient() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _challenge(game);
        _submitLanes(game, 1);
        assertEq(game.proofCount(), 1);
        vm.expectRevert(GameNotOver.selector);
        game.resolve();
    }

    function test_SubmitProofLane_ThresholdFinalizes() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _challenge(game);
        _submitLanes(game, 2);

        game.resolve();
        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));
        // The proposer takes the challenger bond.
        assertEq(game.credit(proposer), PROPOSER_BOND + CHALLENGER_BOND);
        assertEq(game.credit(challengerAccount), 0);
    }

    function test_SubmitProofLane_DuplicateLaneDoesNotCount() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _challenge(game);
        _submitLanes(game, 1);
        vm.expectEmit(true, true, false, true, address(game));
        emit WorldChainProofSystemGame.DuplicateProofLane(
            WorldChainProofLib.ProofLane.VALIDITY_PROOF, game.rootId(), game.proofBitmap()
        );
        game.submitProofLane(0, abi.encodePacked(game.rootId()));
        assertEq(game.proofCount(), 1);
    }

    function test_SubmitProofLane_RejectsInvalidProof() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _challenge(game);
        vm.expectRevert(
            abi.encodeWithSelector(
                WorldChainProofSystemGame.InvalidProof.selector,
                WorldChainProofLib.ProofLane.VALIDITY_PROOF,
                game.rootId()
            )
        );
        game.submitProofLane(0, abi.encodePacked(keccak256("not-the-root-id")));
    }

    function test_SubmitProofLane_RevertsAtProofDeadline() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _challenge(game);
        vm.warp(game.proofDeadline());
        bytes memory proof = abi.encodePacked(game.rootId());
        uint64 deadline = game.proofDeadline();
        vm.expectRevert(
            abi.encodeWithSelector(WorldChainProofSystemGame.ProofPeriodElapsed.selector, block.timestamp, deadline)
        );
        game.submitProofLane(0, proof);
    }

    function test_SubmitProofLane_RejectsInvalidLane() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _challenge(game);
        bytes memory proof = abi.encodePacked(game.rootId());
        vm.expectRevert(abi.encodeWithSelector(WorldChainProofSystemGame.InvalidLane.selector, 3));
        game.submitProofLane(3, proof);
    }

    function test_ProofTimeout_RewardsChallenger() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _challenge(game);
        _submitLanes(game, 1);
        vm.warp(game.proofDeadline());

        game.resolve();
        assertEq(uint8(game.status()), uint8(GameStatus.CHALLENGER_WINS));
        assertEq(uint8(game.invalidationReason()), uint8(WorldChainProofLib.InvalidationReason.PROOF_TIMEOUT));
        assertEq(game.credit(challengerAccount), PROPOSER_BOND + CHALLENGER_BOND);
        assertEq(game.credit(proposer), 0);
    }

    /*//////////////////////////////////////////////////////////////
                          RETRY MECHANICS
    //////////////////////////////////////////////////////////////*/

    function _timedOutGame() internal returns (WorldChainProofSystemGame game) {
        game = _proposeAtAnchor();
        _challenge(game);
        vm.warp(game.proofDeadline());
        game.resolve();
    }

    function test_Retry_AllowedAfterProofTimeout() public {
        WorldChainProofSystemGame first = _timedOutGame();
        WorldChainProofSystemGame retry =
            _propose(type(uint256).max, Claim.unwrap(first.rootClaim()), first.l2SequenceNumber(), 1);
        assertEq(retry.attempt(), 1);
        assertEq(retry.startingRootClaim(), first.startingRootClaim());
    }

    function test_Retry_RevertsWithoutPriorAttempt() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        vm.prank(proposer);
        vm.expectRevert(); // GameNotRetryable(bytes32) with a computed hash argument.
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), _extraData(target, type(uint256).max, 1)
        );
    }

    function test_Retry_RevertsWhilePriorInProgress() public {
        WorldChainProofSystemGame first = _proposeAtAnchor();
        uint256 target = first.l2SequenceNumber();
        Claim claim = first.rootClaim();
        vm.prank(proposer);
        vm.expectRevert();
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, claim, _extraData(target, type(uint256).max, 1));
    }

    function test_Retry_RevertsAfterPriorFinalized() public {
        WorldChainProofSystemGame first = _proposeAtAnchor();
        _resolveUnchallenged(first);
        Claim claim = first.rootClaim();
        uint256 target = first.l2SequenceNumber();
        vm.prank(proposer);
        vm.expectRevert();
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, claim, _extraData(target, type(uint256).max, 1));
    }

    function test_Retry_InheritedInvalidationMustRebase() public {
        // Parent times out; its child is invalidated with INVALID_PARENT and is not retryable
        // under the same parent reference.
        WorldChainProofSystemGame parent = _proposeAtAnchor();
        _challenge(parent);
        WorldChainProofSystemGame child = _proposeChild(0);
        vm.warp(parent.proofDeadline());
        parent.resolve();
        child.resolve();
        assertEq(uint8(child.invalidationReason()), uint8(WorldChainProofLib.InvalidationReason.INVALID_PARENT));

        Claim claim = child.rootClaim();
        uint256 target = child.l2SequenceNumber();
        bytes memory retryExtraData = _extraData(target, 0, 1);
        vm.prank(proposer);
        vm.expectRevert();
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, claim, retryExtraData);
    }

    /*//////////////////////////////////////////////////////////////
                       PARENT-GATED RESOLUTION
    //////////////////////////////////////////////////////////////*/

    function test_Resolve_ChildWaitsForUnresolvedParent() public {
        WorldChainProofSystemGame parent = _proposeAtAnchor();
        WorldChainProofSystemGame child = _proposeChild(0);
        _challenge(child);
        _submitLanes(child, 2);

        // Threshold-ready child still cannot resolve ahead of its parent.
        vm.expectRevert(ParentGameNotResolved.selector);
        child.resolve();

        (bool resolvable, WorldChainProofLib.RootState outcome,) = child.resolutionStatus();
        assertFalse(resolvable);
        assertEq(uint8(outcome), uint8(WorldChainProofLib.RootState.CHALLENGED));

        // Once the parent resolves, the child finalizes on its proofs.
        vm.warp(parent.challengeDeadline());
        parent.resolve();
        child.resolve();
        assertEq(uint8(child.status()), uint8(GameStatus.DEFENDER_WINS));
    }

    function test_Resolve_InvalidParentCascades() public {
        WorldChainProofSystemGame parent = _proposeAtAnchor();
        _challenge(parent);
        WorldChainProofSystemGame child = _proposeChild(0);
        _challenge(child);
        vm.warp(parent.proofDeadline());
        parent.resolve();

        child.resolve();
        assertEq(uint8(child.status()), uint8(GameStatus.CHALLENGER_WINS));
        assertEq(uint8(child.invalidationReason()), uint8(WorldChainProofLib.InvalidationReason.INVALID_PARENT));
        // Neither party is at fault: both bonds refund.
        assertEq(child.credit(proposer), PROPOSER_BOND);
        assertEq(child.credit(challengerAccount), CHALLENGER_BOND);
    }

    function test_Resolve_BlacklistedParentCascadesWithoutParentResolution() public {
        WorldChainProofSystemGame parent = _proposeAtAnchor();
        WorldChainProofSystemGame child = _proposeChild(0);
        vm.prank(guardian);
        asr.blacklistDisputeGame(IDisputeGame(address(parent)));

        (bool resolvable, WorldChainProofLib.RootState outcome, WorldChainProofLib.InvalidationReason reason) =
            child.resolutionStatus();
        assertTrue(resolvable);
        assertEq(uint8(outcome), uint8(WorldChainProofLib.RootState.INVALIDATED));
        assertEq(uint8(reason), uint8(WorldChainProofLib.InvalidationReason.INVALID_PARENT));

        child.resolve();
        assertEq(uint8(child.status()), uint8(GameStatus.CHALLENGER_WINS));
        assertEq(child.credit(proposer), PROPOSER_BOND);
    }

    /*//////////////////////////////////////////////////////////////
                    ASR INTEGRATION / CLOSE / ANCHOR
    //////////////////////////////////////////////////////////////*/

    function test_CloseGame_RevertsBeforeResolution() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        vm.expectRevert(); // GameNotResolved
        game.closeGame();
    }

    function test_CloseGame_RevertsBeforeAirgap() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        assertFalse(asr.isGameFinalized(IDisputeGame(address(game))));
        vm.expectRevert(GameNotFinalized.selector);
        game.closeGame();
    }

    function test_CloseGame_RevertsWhenPaused() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);
        systemConfig.setPaused(true);
        vm.expectRevert(GamePaused.selector);
        game.closeGame();
    }

    function test_CloseGame_AdvancesAnchor() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        assertTrue(asr.isGameRespected(IDisputeGame(address(game))));

        _passAirgap(game);
        assertTrue(asr.isGameClaimValid(IDisputeGame(address(game))));
        game.closeGame();

        (Hash root, uint256 blockNum) = asr.getAnchorRoot();
        assertEq(Hash.unwrap(root), Claim.unwrap(game.rootClaim()));
        assertEq(blockNum, game.l2SequenceNumber());
        assertEq(uint8(game.bondDistributionMode()), uint8(BondDistributionMode.NORMAL));
    }

    function test_CloseGame_IsIdempotent() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);
        game.closeGame();
        game.closeGame();
    }

    function test_Anchor_AdvancesMonotonicallyThroughChain() public {
        WorldChainProofSystemGame parent = _proposeAtAnchor();
        WorldChainProofSystemGame child = _proposeChild(0);
        vm.warp(parent.challengeDeadline());
        parent.resolve();
        vm.warp(child.challengeDeadline());
        child.resolve();

        _passAirgap(child);
        parent.closeGame();
        (, uint256 anchorBlock) = asr.getAnchorRoot();
        assertEq(anchorBlock, parent.l2SequenceNumber());

        child.closeGame();
        (, anchorBlock) = asr.getAnchorRoot();
        assertEq(anchorBlock, child.l2SequenceNumber());
    }

    function test_UnrespectedGame_ResolvesAndPaysButCannotAnchor() public {
        vm.prank(guardian);
        asr.setRespectedGameType(GameType.wrap(999));

        WorldChainProofSystemGame game = _proposeAtAnchor();
        assertFalse(game.wasRespectedGameTypeWhenCreated());

        _resolveUnchallenged(game);
        _passAirgap(game);
        game.closeGame();

        // Anchor unchanged; bonds still distribute normally (the game is proper).
        (, uint256 anchorBlock) = asr.getAnchorRoot();
        assertEq(anchorBlock, STARTING_ANCHOR_BLOCK);
        assertEq(uint8(game.bondDistributionMode()), uint8(BondDistributionMode.NORMAL));
        assertEq(game.credit(proposer), PROPOSER_BOND);
    }

    function test_RespectedGameTypeFlip_IsTheCutoverSwitch() public {
        // Respected snapshot is taken at creation and does not retroactively change.
        vm.prank(guardian);
        asr.setRespectedGameType(GameType.wrap(999));
        WorldChainProofSystemGame before = _proposeAtAnchor();
        assertFalse(before.wasRespectedGameTypeWhenCreated());

        vm.prank(guardian);
        asr.setRespectedGameType(WC_GAME_TYPE);
        WorldChainProofSystemGame retryGame =
            _propose(type(uint256).max, keccak256("other-claim"), STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL, 0);
        assertTrue(retryGame.wasRespectedGameTypeWhenCreated());
        assertFalse(before.wasRespectedGameTypeWhenCreated());
    }

    function test_Create_UnrespectedParentRevertsAfterCutover() public {
        vm.prank(guardian);
        asr.setRespectedGameType(GameType.wrap(999));
        WorldChainProofSystemGame parent = _proposeAtAnchor();

        vm.prank(guardian);
        asr.setRespectedGameType(WC_GAME_TYPE);

        uint256 childTarget = parent.l2SequenceNumber() + BLOCK_INTERVAL;
        bytes memory childExtraData = _extraData(childTarget, 0, 0);
        vm.prank(proposer);
        vm.expectRevert(InvalidParentGame.selector);
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, Claim.wrap(_rootClaimFor(childTarget)), childExtraData);
    }

    function test_Retry_UnrespectedPreCutoverGame() public {
        vm.prank(guardian);
        asr.setRespectedGameType(GameType.wrap(999));
        WorldChainProofSystemGame before = _proposeAtAnchor();

        vm.prank(guardian);
        asr.setRespectedGameType(WC_GAME_TYPE);
        WorldChainProofSystemGame afterCutover =
            _propose(type(uint256).max, Claim.unwrap(before.rootClaim()), before.l2SequenceNumber(), 1);

        assertFalse(before.wasRespectedGameTypeWhenCreated());
        assertTrue(afterCutover.wasRespectedGameTypeWhenCreated());
        assertEq(afterCutover.attempt(), 1);
    }

    function test_Retirement_InvalidatesResolvedLineage() public {
        WorldChainProofSystemGame parent = _proposeAtAnchor();
        WorldChainProofSystemGame child = _proposeChild(0);
        _resolveUnchallenged(parent);
        _resolveUnchallenged(child);

        vm.prank(guardian);
        asr.updateRetirementTimestamp();

        assertTrue(asr.isGameRetired(IDisputeGame(address(parent))));
        assertTrue(asr.isGameRetired(IDisputeGame(address(child))));
        assertFalse(asr.isGameClaimValid(IDisputeGame(address(child))));

        _passAirgap(child);
        child.closeGame();
        assertEq(uint8(child.bondDistributionMode()), uint8(BondDistributionMode.REFUND));
    }

    /*//////////////////////////////////////////////////////////////
                   BOND SETTLEMENT / DELAYEDWETH
    //////////////////////////////////////////////////////////////*/

    function test_ClaimCredit_TwoPhaseWithdrawal() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);

        uint256 balanceBefore = proposer.balance;

        // Phase 1 unlocks; an immediate second call is blocked by the WETH delay.
        game.claimCredit(proposer);
        vm.expectRevert(bytes("DelayedWETH: withdrawal delay not met"));
        game.claimCredit(proposer);

        vm.warp(block.timestamp + WETH_DELAY_SECONDS);
        game.claimCredit(proposer);
        assertEq(proposer.balance, balanceBefore + PROPOSER_BOND);
    }

    function test_ClaimCredit_PermissionlessPaysRecipientNotCaller() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);

        address keeper = makeAddr("keeper");
        uint256 balanceBefore = proposer.balance;
        vm.prank(keeper);
        game.claimCredit(proposer);
        vm.warp(block.timestamp + WETH_DELAY_SECONDS);
        vm.prank(keeper);
        game.claimCredit(proposer);

        assertEq(proposer.balance, balanceBefore + PROPOSER_BOND);
        assertEq(keeper.balance, 0);
    }

    function test_ClaimCredit_ClosesGameWithoutCredit() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);

        // A zero-credit call still closes the game and does not revert.
        game.claimCredit(makeAddr("nobody"));
        assertEq(uint8(game.bondDistributionMode()), uint8(BondDistributionMode.NORMAL));
    }

    function test_Blacklist_AfterResolution_RefundsBothBonds() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _challenge(game);
        _submitLanes(game, 2);
        game.resolve();
        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));

        // The guardian blacklists during the airgap; settlement flips to refund mode.
        vm.prank(guardian);
        asr.blacklistDisputeGame(IDisputeGame(address(game)));
        assertEq(game.credit(proposer), PROPOSER_BOND);
        assertEq(game.credit(challengerAccount), CHALLENGER_BOND);

        _passAirgap(game);
        game.closeGame();
        assertEq(uint8(game.bondDistributionMode()), uint8(BondDistributionMode.REFUND));
        assertEq(game.credit(proposer), PROPOSER_BOND);
        assertEq(game.credit(challengerAccount), CHALLENGER_BOND);

        // The blacklisted game can never advance the anchor.
        (, uint256 anchorBlock) = asr.getAnchorRoot();
        assertEq(anchorBlock, STARTING_ANCHOR_BLOCK);
    }

    function test_Retirement_FlipsSettlementToRefund() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _challenge(game);
        vm.warp(game.proofDeadline());
        game.resolve();

        vm.prank(guardian);
        asr.updateRetirementTimestamp();
        assertTrue(asr.isGameRetired(IDisputeGame(address(game))));

        _passAirgap(game);
        game.closeGame();
        assertEq(uint8(game.bondDistributionMode()), uint8(BondDistributionMode.REFUND));
        // The challenger who won on merits still only recovers their own bond in refund mode.
        assertEq(game.credit(proposer), PROPOSER_BOND);
        assertEq(game.credit(challengerAccount), CHALLENGER_BOND);
    }

    function test_DelayedWETH_GuardianClawback() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _challenge(game);

        // The proxy admin owner (this test) can hold the game's WETH balance.
        uint256 gameBalance = weth.balanceOf(address(game));
        assertEq(gameBalance, PROPOSER_BOND + CHALLENGER_BOND);
        weth.hold(address(game));
        assertEq(weth.balanceOf(address(game)), 0);
        assertEq(weth.balanceOf(address(this)), gameBalance);
    }

    /*//////////////////////////////////////////////////////////////
                        IDisputeGame SURFACE
    //////////////////////////////////////////////////////////////*/

    function test_RootClaimByChainId() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        assertEq(Claim.unwrap(game.rootClaimByChainId(CHAIN_ID)), Claim.unwrap(game.rootClaim()));
        vm.expectRevert(UnknownChainId.selector);
        game.rootClaimByChainId(CHAIN_ID + 1);
    }

    function test_Domain_ExposedForProofLanes() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        WorldChainProofLib.Domain memory d = game.domain();
        assertEq(d.chainId, CHAIN_ID);
        assertEq(d.proofSystemVersion, PROOF_SYSTEM_VERSION);
        assertEq(d.rollupConfigHash, ROLLUP_CONFIG_HASH);
        assertEq(d.blockInterval, BLOCK_INTERVAL);
        assertEq(game.domainHash(), WorldChainProofLib.domainHash(d));
    }

    receive() external payable {}
}
