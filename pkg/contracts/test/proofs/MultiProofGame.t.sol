// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {stdError} from "@forge-std/Test.sol";

import {OPStackFixtures} from "./OPStackFixtures.sol";
import {MultiProofGame} from "../../src/proofs/MultiProofGame.sol";
import {ProofLib} from "../../src/proofs/ProofLib.sol";

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
    UnexpectedGameType
} from "@optimism-bedrock/src/dispute/lib/Errors.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";

contract MultiProofGameTest is OPStackFixtures {
    /*//////////////////////////////////////////////////////////////
                        CREATION / DGF INTEGRATION
    //////////////////////////////////////////////////////////////*/

    function test_Create_CWIARoundTrip() public {
        MultiProofGame game = _proposeAtAnchor();
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;

        assertEq(game.gameCreator(), proposer);
        assertEq(Claim.unwrap(game.rootClaim()), _rootClaimFor(target));
        assertEq(Hash.unwrap(game.l1Head()), blockhash(block.number - 1));
        assertEq(game.l2SequenceNumber(), target);
        assertEq(game.parentIndex(), type(uint256).max);
        assertEq(game.attempt(), 0);
        assertEq(game.parentRef(), address(asr));
        assertEq(game.startingRootClaim(), STARTING_ANCHOR_ROOT);
        assertEq(game.startingL2BlockNumber(), STARTING_ANCHOR_BLOCK);
        assertEq(GameType.unwrap(game.gameType()), GameType.unwrap(WC_GAME_TYPE));
        assertTrue(game.wasRespectedGameTypeWhenCreated());
        assertEq(uint8(game.status()), uint8(GameStatus.IN_PROGRESS));
        assertEq(uint8(game.state()), uint8(ProofLib.RootState.PROPOSED));
        assertEq(game.l1OriginNumber(), block.number - 1);
        assertEq(
            game.rootId(),
            ProofLib.rootId(
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
        emit MultiProofGame.WorldChainGameCreated(bytes32(0), address(0), bytes32(0), 0, bytes32(0), 0, 0, address(0));
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
            abi.encode(target, type(uint256).max, uint256(0), uint256(0))
        );
    }

    function test_Initialize_DirectImplementationCallReverts() public {
        // The bare implementation sees 4-byte calldata: no CWIA payload appended.
        vm.expectRevert(BadExtraData.selector);
        gameImpl.initialize();
    }

    function test_Initialize_ReinitializationReverts() public {
        MultiProofGame game = _proposeAtAnchor();
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
                MultiProofGame.InvalidL2BlockNumber.selector, STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL, target
            )
        );
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), _extraData(target, type(uint256).max, 0)
        );
    }

    function test_Create_OutOfRangeParentIndexReverts() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        vm.prank(proposer);
        vm.expectRevert(stdError.indexOOBError);
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), _extraData(target, 5, 0));
    }

    function test_Create_ParentOfDifferentGameTypeReverts() public {
        // Register a second implementation under game type 43 and create a game with it.
        GameType otherType = GameType.wrap(43);
        MultiProofGame otherImpl = new MultiProofGame(_gameConfig(otherType));
        dgf.setImplementation(otherType, IDisputeGame(address(otherImpl)));
        dgf.setInitBond(otherType, PROPOSER_BOND);
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        vm.prank(proposer);
        dgf.create{value: PROPOSER_BOND}(
            otherType, Claim.wrap(_rootClaimFor(target)), _extraData(target, type(uint256).max, 0)
        );

        // Chaining a WC_GAME_TYPE child onto the type-43 game at index 0 must fail.
        uint256 childTarget = target + BLOCK_INTERVAL;
        vm.prank(proposer);
        vm.expectRevert(UnexpectedGameType.selector);
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(childTarget)), _extraData(childTarget, 0, 0)
        );
    }

    function test_Create_InvalidatedParentReverts() public {
        MultiProofGame parent = _proposeAtAnchor();
        _challenge(parent);
        vm.warp(parent.proofDeadline());
        parent.resolve();
        assertEq(uint8(parent.status()), uint8(GameStatus.CHALLENGER_WINS));

        uint256 childTarget = parent.l2SequenceNumber() + BLOCK_INTERVAL;
        vm.prank(proposer);
        vm.expectRevert(InvalidParentGame.selector);
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(childTarget)), _extraData(childTarget, 0, 0)
        );
    }

    function test_Create_BlacklistedParentReverts() public {
        MultiProofGame parent = _proposeAtAnchor();
        vm.prank(guardian);
        asr.blacklistDisputeGame(IDisputeGame(address(parent)));

        uint256 childTarget = parent.l2SequenceNumber() + BLOCK_INTERVAL;
        vm.prank(proposer);
        vm.expectRevert(InvalidParentGame.selector);
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(childTarget)), _extraData(childTarget, 0, 0)
        );
    }

    function test_Create_ParentAtOrBelowAnchorRequiresSentinel() public {
        // Finalize the first game and advance the anchor to it.
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);
        game.closeGame();
        (, uint256 anchorBlock) = asr.getAnchorRoot();
        assertEq(anchorBlock, game.l2SequenceNumber());

        // Chaining onto the now-anchored game by index is rejected as stale...
        uint256 childTarget = game.l2SequenceNumber() + BLOCK_INTERVAL;
        vm.prank(proposer);
        vm.expectRevert(InvalidParentGame.selector);
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(childTarget)), _extraData(childTarget, 0, 0)
        );

        // ...while the anchor sentinel path proposes the same transition.
        MultiProofGame child = _proposeAtAnchor();
        assertEq(child.startingRootClaim(), Claim.unwrap(game.rootClaim()));
        assertEq(child.startingL2BlockNumber(), game.l2SequenceNumber());
    }

    /*//////////////////////////////////////////////////////////////
                          CONSTRUCTOR VALIDATION
    //////////////////////////////////////////////////////////////*/

    function test_Constructor_RejectsOutOfRangeThreshold() public {
        MultiProofGame.GameConfig memory config = _gameConfig(WC_GAME_TYPE);
        config.proofThreshold = 0;
        vm.expectRevert(MultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);

        config.proofThreshold = PROOF_LANE_COUNT_PLUS_ONE();
        vm.expectRevert(MultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);
    }

    function PROOF_LANE_COUNT_PLUS_ONE() internal pure returns (uint8) {
        return ProofLib.PROOF_LANE_COUNT + 1;
    }

    function test_Constructor_RejectsZeroParameters() public {
        MultiProofGame.GameConfig memory config = _gameConfig(WC_GAME_TYPE);
        config.challengePeriod = 0;
        vm.expectRevert(MultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);

        config = _gameConfig(WC_GAME_TYPE);
        config.domain.blockInterval = 0;
        vm.expectRevert(MultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);
    }

    /*//////////////////////////////////////////////////////////////
                          CHALLENGE WINDOW
    //////////////////////////////////////////////////////////////*/

    function test_UnchallengedGame_FinalizesAfterChallengeWindow() public {
        MultiProofGame game = _proposeAtAnchor();

        vm.expectEmit(true, false, false, false, address(game));
        emit IDisputeGame.Resolved(GameStatus.DEFENDER_WINS);
        _resolveUnchallenged(game);

        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));
        assertEq(uint8(game.state()), uint8(ProofLib.RootState.FINALIZED));
        assertEq(game.finalizedAt(), uint64(block.timestamp));
        assertEq(game.credit(proposer), PROPOSER_BOND);
    }

    function test_Resolve_RevertsBeforeChallengeDeadline() public {
        MultiProofGame game = _proposeAtAnchor();
        vm.expectRevert(GameNotOver.selector);
        game.resolve();
    }

    function test_Resolve_RevertsWhenAlreadyResolved() public {
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        vm.expectRevert(ClaimAlreadyResolved.selector);
        game.resolve();
    }

    function test_Challenge_RevertsForUnstaked() public {
        MultiProofGame game = _proposeAtAnchor();
        address unstaked = makeAddr("unstaked");
        vm.deal(unstaked, 1 ether);
        vm.prank(unstaked);
        vm.expectRevert(abi.encodeWithSelector(MultiProofGame.UnstakedChallenger.selector, unstaked));
        game.challenge{value: CHALLENGER_BOND}();
    }

    function test_Challenge_SucceedsForStaked() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        assertEq(game.challenger(), challengerAccount);
        assertEq(uint8(game.state()), uint8(ProofLib.RootState.CHALLENGED));
        assertEq(game.proofDeadline(), uint64(block.timestamp + PROOF_PERIOD));
        assertEq(weth.balanceOf(address(game)), PROPOSER_BOND + CHALLENGER_BOND);
    }

    function test_Challenge_RevertsAtOrAfterDeadline() public {
        MultiProofGame game = _proposeAtAnchor();
        vm.warp(game.challengeDeadline());
        vm.prank(challengerAccount);
        vm.expectRevert(
            abi.encodeWithSelector(
                MultiProofGame.ChallengePeriodElapsed.selector, block.timestamp, game.challengeDeadline()
            )
        );
        game.challenge{value: CHALLENGER_BOND}();
    }

    function test_Challenge_RevertsOnSecondChallenge() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        stakingRegistry.setStaked(address(this), true);
        vm.expectRevert(ClaimAlreadyChallenged.selector);
        game.challenge{value: CHALLENGER_BOND}();
    }

    function test_Challenge_RevertsForWrongBond() public {
        MultiProofGame game = _proposeAtAnchor();
        vm.prank(challengerAccount);
        vm.expectRevert(IncorrectBondAmount.selector);
        game.challenge{value: CHALLENGER_BOND - 1}();
    }

    /*//////////////////////////////////////////////////////////////
                        PROOF LANES / THRESHOLD
    //////////////////////////////////////////////////////////////*/

    function test_SubmitProofLane_RevertsWhenUnchallenged() public {
        MultiProofGame game = _proposeAtAnchor();
        bytes memory proof = abi.encodePacked(game.rootId());
        vm.expectRevert(ClaimAlreadyResolved.selector);
        game.submitProofLane(0, proof);
    }

    function test_SubmitProofLane_OneLaneInsufficient() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        _submitLanes(game, 1);
        assertEq(game.proofCount(), 1);
        vm.expectRevert(GameNotOver.selector);
        game.resolve();
    }

    function test_SubmitProofLane_ThresholdFinalizes() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        _submitLanes(game, 2);

        game.resolve();
        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));
        // The proposer takes the challenger bond.
        assertEq(game.credit(proposer), PROPOSER_BOND + CHALLENGER_BOND);
        assertEq(game.credit(challengerAccount), 0);
    }

    function test_SubmitProofLane_DuplicateLaneDoesNotCount() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        _submitLanes(game, 1);
        vm.expectEmit(true, true, false, true, address(game));
        emit MultiProofGame.DuplicateProofLane(ProofLib.ProofLane.VALIDITY_PROOF, game.rootId(), game.proofBitmap());
        game.submitProofLane(0, abi.encodePacked(game.rootId()));
        assertEq(game.proofCount(), 1);
    }

    function test_SubmitProofLane_RejectsInvalidProof() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        vm.expectRevert(
            abi.encodeWithSelector(
                MultiProofGame.InvalidProof.selector, ProofLib.ProofLane.VALIDITY_PROOF, game.rootId()
            )
        );
        game.submitProofLane(0, abi.encodePacked(keccak256("not-the-root-id")));
    }

    function test_SubmitProofLane_RevertsAtProofDeadline() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        vm.warp(game.proofDeadline());
        bytes memory proof = abi.encodePacked(game.rootId());
        uint64 deadline = game.proofDeadline();
        vm.expectRevert(abi.encodeWithSelector(MultiProofGame.ProofPeriodElapsed.selector, block.timestamp, deadline));
        game.submitProofLane(0, proof);
    }

    function test_SubmitProofLane_RejectsInvalidLane() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        bytes memory proof = abi.encodePacked(game.rootId());
        vm.expectRevert(abi.encodeWithSelector(MultiProofGame.InvalidLane.selector, 3));
        game.submitProofLane(3, proof);
    }

    function test_ProofTimeout_RewardsChallenger() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        _submitLanes(game, 1);
        vm.warp(game.proofDeadline());

        game.resolve();
        assertEq(uint8(game.status()), uint8(GameStatus.CHALLENGER_WINS));
        assertEq(uint8(game.invalidationReason()), uint8(ProofLib.InvalidationReason.PROOF_TIMEOUT));
        assertEq(game.credit(challengerAccount), PROPOSER_BOND + CHALLENGER_BOND);
        assertEq(game.credit(proposer), 0);
    }

    /*//////////////////////////////////////////////////////////////
                          RETRY MECHANICS
    //////////////////////////////////////////////////////////////*/

    function _timedOutGame() internal returns (MultiProofGame game) {
        game = _proposeAtAnchor();
        _challenge(game);
        vm.warp(game.proofDeadline());
        game.resolve();
    }

    function test_Retry_AllowedAfterProofTimeout() public {
        MultiProofGame first = _timedOutGame();
        MultiProofGame retry = _propose(type(uint256).max, Claim.unwrap(first.rootClaim()), first.l2SequenceNumber(), 1);
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
        MultiProofGame first = _proposeAtAnchor();
        uint256 target = first.l2SequenceNumber();
        Claim claim = first.rootClaim();
        vm.prank(proposer);
        vm.expectRevert();
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, claim, _extraData(target, type(uint256).max, 1));
    }

    function test_Retry_RevertsAfterPriorFinalized() public {
        MultiProofGame first = _proposeAtAnchor();
        _resolveUnchallenged(first);
        Claim claim = first.rootClaim();
        uint256 target = first.l2SequenceNumber();
        vm.prank(proposer);
        vm.expectRevert();
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, claim, _extraData(target, type(uint256).max, 1));
    }

    function test_Retry_InheritedInvalidationMustRebase() public {
        // Parent times out; its child is invalidated with INVALID_PARENT and is not retryable
        // under the same parent index.
        MultiProofGame parent = _proposeAtAnchor();
        _challenge(parent);
        MultiProofGame child = _proposeChild(0);
        vm.warp(parent.proofDeadline());
        parent.resolve();
        child.resolve();
        assertEq(uint8(child.invalidationReason()), uint8(ProofLib.InvalidationReason.INVALID_PARENT));

        Claim claim = child.rootClaim();
        uint256 target = child.l2SequenceNumber();
        vm.prank(proposer);
        vm.expectRevert();
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, claim, _extraData(target, 0, 1));
    }

    /*//////////////////////////////////////////////////////////////
                       PARENT-GATED RESOLUTION
    //////////////////////////////////////////////////////////////*/

    function test_Resolve_ChildWaitsForUnresolvedParent() public {
        MultiProofGame parent = _proposeAtAnchor();
        MultiProofGame child = _proposeChild(0);
        _challenge(child);
        _submitLanes(child, 2);

        // Threshold-ready child still cannot resolve ahead of its parent.
        vm.expectRevert(ParentGameNotResolved.selector);
        child.resolve();

        (bool resolvable, ProofLib.RootState outcome,) = child.resolutionStatus();
        assertFalse(resolvable);
        assertEq(uint8(outcome), uint8(ProofLib.RootState.CHALLENGED));

        // Once the parent resolves, the child finalizes on its proofs.
        vm.warp(parent.challengeDeadline());
        parent.resolve();
        child.resolve();
        assertEq(uint8(child.status()), uint8(GameStatus.DEFENDER_WINS));
    }

    function test_Resolve_InvalidParentCascades() public {
        MultiProofGame parent = _proposeAtAnchor();
        _challenge(parent);
        MultiProofGame child = _proposeChild(0);
        _challenge(child);
        vm.warp(parent.proofDeadline());
        parent.resolve();

        child.resolve();
        assertEq(uint8(child.status()), uint8(GameStatus.CHALLENGER_WINS));
        assertEq(uint8(child.invalidationReason()), uint8(ProofLib.InvalidationReason.INVALID_PARENT));
        // Neither party is at fault: both bonds refund.
        assertEq(child.credit(proposer), PROPOSER_BOND);
        assertEq(child.credit(challengerAccount), CHALLENGER_BOND);
    }

    function test_Resolve_BlacklistedParentCascadesWithoutParentResolution() public {
        MultiProofGame parent = _proposeAtAnchor();
        MultiProofGame child = _proposeChild(0);
        vm.prank(guardian);
        asr.blacklistDisputeGame(IDisputeGame(address(parent)));

        (bool resolvable, ProofLib.RootState outcome, ProofLib.InvalidationReason reason) = child.resolutionStatus();
        assertTrue(resolvable);
        assertEq(uint8(outcome), uint8(ProofLib.RootState.INVALIDATED));
        assertEq(uint8(reason), uint8(ProofLib.InvalidationReason.INVALID_PARENT));

        child.resolve();
        assertEq(uint8(child.status()), uint8(GameStatus.CHALLENGER_WINS));
        assertEq(child.credit(proposer), PROPOSER_BOND);
    }

    /*//////////////////////////////////////////////////////////////
                    ASR INTEGRATION / CLOSE / ANCHOR
    //////////////////////////////////////////////////////////////*/

    function test_CloseGame_RevertsBeforeResolution() public {
        MultiProofGame game = _proposeAtAnchor();
        vm.expectRevert(); // GameNotResolved
        game.closeGame();
    }

    function test_CloseGame_RevertsBeforeAirgap() public {
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        assertFalse(asr.isGameFinalized(IDisputeGame(address(game))));
        vm.expectRevert(GameNotFinalized.selector);
        game.closeGame();
    }

    function test_CloseGame_RevertsWhenPaused() public {
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);
        systemConfig.setPaused(true);
        vm.expectRevert(GamePaused.selector);
        game.closeGame();
    }

    function test_CloseGame_AdvancesAnchor() public {
        MultiProofGame game = _proposeAtAnchor();
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
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);
        game.closeGame();
        game.closeGame();
    }

    function test_Anchor_AdvancesMonotonicallyThroughChain() public {
        MultiProofGame parent = _proposeAtAnchor();
        MultiProofGame child = _proposeChild(0);
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

        MultiProofGame game = _proposeAtAnchor();
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
        MultiProofGame before = _proposeAtAnchor();
        assertFalse(before.wasRespectedGameTypeWhenCreated());

        vm.prank(guardian);
        asr.setRespectedGameType(WC_GAME_TYPE);
        MultiProofGame retryGame =
            _propose(type(uint256).max, keccak256("other-claim"), STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL, 0);
        assertTrue(retryGame.wasRespectedGameTypeWhenCreated());
        assertFalse(before.wasRespectedGameTypeWhenCreated());
    }

    /*//////////////////////////////////////////////////////////////
                   BOND SETTLEMENT / DELAYEDWETH
    //////////////////////////////////////////////////////////////*/

    function test_ClaimCredit_TwoPhaseWithdrawal() public {
        MultiProofGame game = _proposeAtAnchor();
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
        MultiProofGame game = _proposeAtAnchor();
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
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);

        // A zero-credit call still closes the game and does not revert.
        game.claimCredit(makeAddr("nobody"));
        assertEq(uint8(game.bondDistributionMode()), uint8(BondDistributionMode.NORMAL));
    }

    function test_Blacklist_AfterResolution_RefundsBothBonds() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        _submitLanes(game, 2);
        game.resolve();
        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));

        // The guardian blacklists during the airgap; settlement flips to refund mode.
        vm.prank(guardian);
        asr.blacklistDisputeGame(IDisputeGame(address(game)));
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
        MultiProofGame game = _proposeAtAnchor();
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
        MultiProofGame game = _proposeAtAnchor();
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
        MultiProofGame game = _proposeAtAnchor();
        assertEq(Claim.unwrap(game.rootClaimByChainId(CHAIN_ID)), Claim.unwrap(game.rootClaim()));
        // `IDisputeGame` declares the getter `pure`, so the argument is ignored rather than
        // checked against the domain chain ID.
        assertEq(Claim.unwrap(game.rootClaimByChainId(CHAIN_ID + 1)), Claim.unwrap(game.rootClaim()));
    }

    function test_Domain_ExposedForProofLanes() public {
        MultiProofGame game = _proposeAtAnchor();
        ProofLib.Domain memory d = game.domain();
        assertEq(d.chainId, CHAIN_ID);
        assertEq(d.proofSystemVersion, PROOF_SYSTEM_VERSION);
        assertEq(d.rollupConfigHash, ROLLUP_CONFIG_HASH);
        assertEq(d.blockInterval, BLOCK_INTERVAL);
        assertEq(game.domainHash(), ProofLib.domainHash(d));
    }

    receive() external payable {}
}
