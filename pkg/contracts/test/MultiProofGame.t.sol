// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {OPStackFixtures} from "./proofs/OPStackFixtures.sol";
import {MultiProofGame} from "../src/proofs/MultiProofGame.sol";
import {IMultiProofGame} from "../src/proofs/interfaces/IMultiProofGame.sol";
import {ProofLib} from "../src/proofs/lib/ProofLib.sol";

import {BondDistributionMode, Claim, GameStatus, GameType, Hash} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {
    BadExtraData,
    ClaimAlreadyChallenged,
    GameNotFinalized,
    GameNotOver,
    GamePaused,
    IncorrectBondAmount,
    InvalidParentGame,
    ParentGameNotResolved
} from "@optimism-bedrock/src/dispute/lib/Errors.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";

contract MultiProofGameTest is OPStackFixtures {
    function test_Create_RegistersCanonicalGame() public {
        MultiProofGame game = _proposeAtAnchor();
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;

        assertEq(game.gameCreator(), proposer);
        assertEq(Claim.unwrap(game.rootClaim()), _rootClaimFor(target));
        assertEq(game.l2SequenceNumber(), target);
        assertEq(game.parentRef(), address(asr));
        assertEq(game.startingRootClaim(), STARTING_ANCHOR_ROOT);
        assertEq(game.startingL2BlockNumber(), STARTING_ANCHOR_BLOCK);
        assertEq(game.attempt(), 0);
        assertEq(GameType.unwrap(game.gameType()), GameType.unwrap(WC_GAME_TYPE));
        assertTrue(game.wasRespectedGameTypeWhenCreated());

        (GameType gameType_, Claim rootClaim_, bytes memory extraData_) = game.gameData();
        (IDisputeGame registered,) = dgf.games(gameType_, rootClaim_, extraData_);
        assertEq(address(registered), address(game));
        assertTrue(asr.isGameRegistered(IDisputeGame(address(game))));
        assertEq(weth.balanceOf(address(game)), PROPOSER_BOND);
        assertEq(game.proofBitmap(), 0);
    }

    function test_Create_RejectsMalformedExtraData() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;

        vm.prank(proposer);
        vm.expectRevert(BadExtraData.selector);
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), abi.encode(target, address(asr))
        );

        uint256 malformedParent = uint256(uint160(address(asr))) | (uint256(1) << 160);
        bytes memory extraData = abi.encode(ProofLib.domainHash(_domain()), target, malformedParent, uint256(0));
        vm.prank(proposer);
        vm.expectRevert(BadExtraData.selector);
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), extraData);
    }

    function test_Create_RejectsWrongDomainAndInterval() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        bytes32 wrongDomain = keccak256("wrong-domain");

        vm.prank(proposer);
        vm.expectRevert(
            abi.encodeWithSelector(IMultiProofGame.InvalidDomainHash.selector, gameImpl.domainHash(), wrongDomain)
        );
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), abi.encode(wrongDomain, target, address(asr), uint256(0))
        );

        uint256 wrongTarget = target + 1;
        vm.prank(proposer);
        vm.expectRevert(abi.encodeWithSelector(IMultiProofGame.InvalidL2BlockNumber.selector, target, wrongTarget));
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(wrongTarget)), _extraData(wrongTarget, type(uint256).max, 0)
        );
    }

    function test_Create_RejectsUnregisteredAndBlacklistedParents() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        vm.prank(proposer);
        vm.expectRevert();
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), _extraDataForParent(target, makeAddr("unknown-parent"), 0)
        );

        MultiProofGame parent = _proposeAtAnchor();
        vm.prank(guardian);
        asr.blacklistDisputeGame(IDisputeGame(address(parent)));

        uint256 childTarget = parent.l2SequenceNumber() + BLOCK_INTERVAL;
        bytes memory childExtraData = _extraData(childTarget, 0, 0);
        vm.prank(proposer);
        vm.expectRevert(InvalidParentGame.selector);
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, Claim.wrap(_rootClaimFor(childTarget)), childExtraData);
    }

    function test_Create_RejectsParentFromAnotherGameType() public {
        GameType otherType = GameType.wrap(43);
        MultiProofGame otherImpl = new MultiProofGame(_gameConfig());
        dgf.setImplementation(otherType, IDisputeGame(address(otherImpl)), hex"");
        dgf.setInitBond(otherType, PROPOSER_BOND);

        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        vm.prank(proposer);
        dgf.create{value: PROPOSER_BOND}(
            otherType, Claim.wrap(_rootClaimFor(target)), _extraData(target, type(uint256).max, 0)
        );

        uint256 childTarget = target + BLOCK_INTERVAL;
        bytes memory childExtraData = _extraData(childTarget, 0, 0);
        vm.prank(proposer);
        vm.expectRevert();
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, Claim.wrap(_rootClaimFor(childTarget)), childExtraData);
    }

    function test_Create_UsesPreviousAnchorGameAfterAnchorAdvances() public {
        MultiProofGame parent = _proposeAtAnchor();
        _resolveUnchallenged(parent);
        _passAirgap(parent);
        parent.closeGame();

        uint256 target = parent.l2SequenceNumber() + BLOCK_INTERVAL;
        vm.prank(proposer);
        vm.expectRevert(
            abi.encodeWithSelector(
                IMultiProofGame.InvalidL2BlockNumber.selector, STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL, target
            )
        );
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), _extraData(target, type(uint256).max, 0)
        );

        bytes memory previousAnchorParentExtraData = _extraData(target, 0, 0);
        vm.prank(proposer);
        MultiProofGame previousAnchorChild = MultiProofGame(
            address(
                dgf.create{value: PROPOSER_BOND}(
                    WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), previousAnchorParentExtraData
                )
            )
        );
        assertEq(previousAnchorChild.parentRef(), address(parent));
        assertEq(previousAnchorChild.startingRootClaim(), Claim.unwrap(parent.rootClaim()));
        assertEq(previousAnchorChild.startingL2BlockNumber(), parent.l2SequenceNumber());
    }

    function test_Constructor_RejectsInvalidConfiguration() public {
        IMultiProofGame.GameConfig memory config = _gameConfig();
        config.proofThreshold = 0;
        vm.expectRevert(IMultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);

        config = _gameConfig();
        config.proofPeriod = config.challengePeriod;
        vm.expectRevert(IMultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);

        config = _gameConfig();
        config.protocolFeeRecipient = address(0);
        vm.expectRevert(IMultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);

        config = _gameConfig();
        config.challengeFee = 0;
        vm.expectRevert(IMultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);

        config = _gameConfig();
        config.challengeFee = CHALLENGER_BOND + 1;
        vm.expectRevert(IMultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);

        config = _gameConfig();
        config.proposerBond = CHALLENGE_FEE;
        vm.expectRevert(IMultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);

        config = _gameConfig();
        config.domain.chainId = CHAIN_ID + 1;
        vm.expectRevert(IMultiProofGame.InconsistentSystemConfiguration.selector);
        new MultiProofGame(config);
    }

    function test_UnchallengedFlow_AnyProofLaneCanFinalize() public {
        for (uint8 lane; lane < ProofLib.PROOF_LANE_COUNT; lane++) {
            (, uint256 anchorBlock) = asr.getAnchorRoot();
            uint256 target = anchorBlock + BLOCK_INTERVAL;
            bytes32 rootClaim = keccak256(abi.encode("lane", lane));
            MultiProofGame game = _propose(type(uint256).max, rootClaim, target, 0);

            game.submitProofLane(lane, abi.encodePacked(game.rootId()));
            vm.warp(game.challengeDeadline());
            game.resolve();

            assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));
            assertEq(game.credit(proposer), PROPOSER_BOND);
        }
    }

    function test_UnchallengedFlow_ThresholdProvidesFastFinality() public {
        MultiProofGame game = _proposeAtAnchor();
        game.submitProofLane(0, abi.encodePacked(game.rootId()));

        (bool resolvable,,) = game.resolutionStatus();
        assertFalse(resolvable);
        vm.expectRevert(GameNotOver.selector);
        game.resolve();

        game.submitProofLane(1, abi.encodePacked(game.rootId()));
        ProofLib.RootState rootState;
        ProofLib.InvalidationReason reason;
        (resolvable, rootState, reason) = game.resolutionStatus();
        assertTrue(resolvable);
        assertEq(uint8(rootState), uint8(ProofLib.RootState.FINALIZED));
        assertEq(uint8(reason), uint8(ProofLib.InvalidationReason.NONE));

        game.resolve();
        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));
        assertEq(game.credit(proposer), PROPOSER_BOND);
        assertLt(block.timestamp, game.challengeDeadline());
    }

    function test_UnchallengedFlow_ProoflessProposalLosesBondAndCanRetry() public {
        MultiProofGame first = _proposeAtAnchor();
        vm.warp(first.challengeDeadline());

        (bool resolvable, ProofLib.RootState rootState, ProofLib.InvalidationReason reason) = first.resolutionStatus();
        assertTrue(resolvable);
        assertEq(uint8(rootState), uint8(ProofLib.RootState.INVALIDATED));
        assertEq(uint8(reason), uint8(ProofLib.InvalidationReason.PROOF_TIMEOUT));

        first.resolve();
        assertEq(uint8(first.status()), uint8(GameStatus.CHALLENGER_WINS));
        assertEq(uint8(first.invalidationReason()), uint8(ProofLib.InvalidationReason.PROOF_TIMEOUT));
        assertEq(first.credit(protocolFeeRecipient), PROPOSER_BOND);

        _passAirgap(first);
        uint256 timeoutRecipientBalance = protocolFeeRecipient.balance;
        first.claimCredit(protocolFeeRecipient);
        vm.warp(block.timestamp + WETH_DELAY_SECONDS);
        first.claimCredit(protocolFeeRecipient);
        assertEq(protocolFeeRecipient.balance, timeoutRecipientBalance + PROPOSER_BOND);

        MultiProofGame retry =
            _propose(type(uint256).max, Claim.unwrap(first.rootClaim()), first.l2SequenceNumber(), first.attempt() + 1);
        assertEq(retry.attempt(), 1);
    }

    function test_UnchallengedFlow_AnchorsAndPaysProposer() public {
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);

        vm.expectRevert(GameNotFinalized.selector);
        game.closeGame();

        _passAirgap(game);
        uint256 balanceBefore = proposer.balance;
        address keeper = makeAddr("keeper");
        vm.prank(keeper);
        game.claimCredit(proposer);

        (Hash anchorRoot, uint256 anchorBlock) = asr.getAnchorRoot();
        assertEq(Hash.unwrap(anchorRoot), Claim.unwrap(game.rootClaim()));
        assertEq(anchorBlock, game.l2SequenceNumber());
        assertEq(uint8(game.bondDistributionMode()), uint8(BondDistributionMode.NORMAL));

        vm.warp(block.timestamp + WETH_DELAY_SECONDS);
        vm.prank(keeper);
        game.claimCredit(proposer);
        assertEq(proposer.balance, balanceBefore + PROPOSER_BOND);
        assertEq(keeper.balance, 0);
    }

    function test_Pause_BlocksCreationAndSettlement() public {
        systemConfig.setPaused(true);
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        vm.prank(proposer);
        vm.expectRevert(GamePaused.selector);
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), _extraData(target, type(uint256).max, 0)
        );

        systemConfig.setPaused(false);
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);
        systemConfig.setPaused(true);
        vm.expectRevert(GamePaused.selector);
        game.closeGame();
    }

    function test_Challenge_RequiresStakeBondAndOpenWindow() public {
        MultiProofGame game = _proposeAtAnchor();
        address unstaked = makeAddr("unstaked");
        vm.deal(unstaked, CHALLENGER_BOND);

        vm.prank(unstaked);
        vm.expectRevert(abi.encodeWithSelector(IMultiProofGame.UnstakedChallenger.selector, unstaked));
        game.challenge{value: CHALLENGER_BOND}();

        vm.prank(challengerAccount);
        vm.expectRevert(IncorrectBondAmount.selector);
        game.challenge{value: CHALLENGER_BOND - 1}();

        _challenge(game);
        assertEq(game.refundModeCredit(challengerAccount), CHALLENGER_BOND - CHALLENGE_FEE);
        assertEq(game.refundModeCredit(protocolFeeRecipient), CHALLENGE_FEE);
        assertEq(game.normalModeCredit(protocolFeeRecipient), CHALLENGE_FEE);
        assertEq(weth.balanceOf(address(game)), PROPOSER_BOND + CHALLENGER_BOND);

        vm.prank(challengerAccount);
        vm.expectRevert(ClaimAlreadyChallenged.selector);
        game.challenge{value: CHALLENGER_BOND}();
    }

    function test_Challenge_DoesNotExtendProofDeadline() public {
        MultiProofGame game = _proposeAtAnchor();
        uint64 proofDeadline = game.proofDeadline();
        vm.warp(game.challengeDeadline() - 1);
        _challenge(game);

        assertEq(game.proofDeadline(), proofDeadline);
        assertEq(game.challengedAt(), uint64(block.timestamp));
        assertLt(game.proofDeadline() - game.challengedAt(), game.proofPeriod());
    }

    function test_ProofThreshold_DefenderWinsAndDuplicateDoesNotCount() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);

        game.submitProofLane(0, abi.encodePacked(game.rootId()));
        game.submitProofLane(0, abi.encodePacked(game.rootId()));
        assertEq(game.proofCount(), 1);

        game.submitProofLane(1, abi.encodePacked(game.rootId()));
        game.resolve();
        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));
        assertEq(game.credit(proposer), PROPOSER_BOND + CHALLENGER_BOND - CHALLENGE_FEE);
        assertEq(game.credit(protocolFeeRecipient), CHALLENGE_FEE);
    }

    function test_SelfChallenge_CannotRecycleChallengeFee() public {
        stakingRegistry.setStaked(proposer, true);
        MultiProofGame game = _proposeAtAnchor();

        vm.prank(proposer);
        game.challenge{value: CHALLENGER_BOND}();
        _submitLanes(game, 2);
        game.resolve();

        assertEq(game.credit(proposer), PROPOSER_BOND + CHALLENGER_BOND - CHALLENGE_FEE);
        assertEq(game.credit(protocolFeeRecipient), CHALLENGE_FEE);
        assertEq(game.credit(proposer) + game.credit(protocolFeeRecipient), game.totalBonds());
    }

    function test_ProtocolFeeRecipientOverlapWithProposer_PreservesAllCredit() public {
        IMultiProofGame.GameConfig memory config = _gameConfig();
        config.protocolFeeRecipient = proposer;
        MultiProofGame overlappingImpl = new MultiProofGame(config);
        dgf.setImplementation(WC_GAME_TYPE, IDisputeGame(address(overlappingImpl)), hex"");

        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        _submitLanes(game, 2);
        game.resolve();

        assertEq(game.credit(proposer), game.totalBonds());
    }

    function test_ProtocolFeeRecipientOverlapWithChallenger_PreservesAllCredit() public {
        IMultiProofGame.GameConfig memory config = _gameConfig();
        config.protocolFeeRecipient = challengerAccount;
        MultiProofGame overlappingImpl = new MultiProofGame(config);
        dgf.setImplementation(WC_GAME_TYPE, IDisputeGame(address(overlappingImpl)), hex"");

        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        vm.warp(game.proofDeadline());
        game.resolve();

        assertEq(game.credit(challengerAccount), game.totalBonds());
    }

    function test_Challenge_AfterInitialProofStillRequiresThreshold() public {
        MultiProofGame game = _proposeAtAnchor();
        game.submitProofLane(1, abi.encodePacked(game.rootId()));
        _challenge(game);

        (bool resolvable,,) = game.resolutionStatus();
        assertFalse(resolvable);

        game.submitProofLane(0, abi.encodePacked(game.rootId()));
        game.resolve();
        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));
    }

    function test_ProofLane_RejectsInvalidProofAndExpiredSubmission() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);

        vm.expectRevert();
        game.submitProofLane(0, abi.encodePacked(keccak256("wrong-root")));

        vm.warp(game.proofDeadline());
        bytes memory proof = abi.encodePacked(game.rootId());
        vm.expectRevert();
        game.submitProofLane(0, proof);
    }

    function test_ProofLane_RejectsInitialProofAtChallengeDeadline() public {
        MultiProofGame game = _proposeAtAnchor();
        uint64 deadline = game.challengeDeadline();
        bytes memory proof = abi.encodePacked(game.rootId());
        vm.warp(deadline);

        vm.expectRevert(abi.encodeWithSelector(IMultiProofGame.ProofPeriodElapsed.selector, block.timestamp, deadline));
        game.submitProofLane(0, proof);
    }

    function test_ProofTimeout_ChallengerWinsAndRetryIsAllowed() public {
        MultiProofGame first = _proposeAtAnchor();
        _challenge(first);
        vm.warp(first.proofDeadline());
        first.resolve();

        assertEq(uint8(first.status()), uint8(GameStatus.CHALLENGER_WINS));
        assertEq(uint8(first.invalidationReason()), uint8(ProofLib.InvalidationReason.PROOF_TIMEOUT));
        assertEq(first.credit(challengerAccount), PROPOSER_BOND + CHALLENGER_BOND - CHALLENGE_FEE);
        assertEq(first.credit(protocolFeeRecipient), CHALLENGE_FEE);

        MultiProofGame retry = _propose(type(uint256).max, Claim.unwrap(first.rootClaim()), first.l2SequenceNumber(), 1);
        assertEq(retry.attempt(), 1);
        assertEq(retry.startingRootClaim(), first.startingRootClaim());
    }

    function test_Retry_RejectsInProgressPreviousAttempt() public {
        MultiProofGame first = _proposeAtAnchor();
        Claim claim = first.rootClaim();
        uint256 l2BlockNumber = first.l2SequenceNumber();
        bytes memory retryExtraData = _extraData(l2BlockNumber, type(uint256).max, 1);
        vm.prank(proposer);
        vm.expectRevert();
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, claim, retryExtraData);
    }

    function test_ChildWaitsForParentThenFinalizes() public {
        MultiProofGame parent = _proposeAtAnchor();
        MultiProofGame child = _proposeChild(0);
        _challenge(child);
        _submitLanes(child, 2);

        vm.expectRevert(ParentGameNotResolved.selector);
        child.resolve();

        _resolveUnchallenged(parent);
        child.resolve();
        assertEq(uint8(child.status()), uint8(GameStatus.DEFENDER_WINS));
    }

    function test_InvalidParent_CascadesAndRefundsChildBonds() public {
        MultiProofGame parent = _proposeAtAnchor();
        _challenge(parent);
        MultiProofGame child = _proposeChild(0);
        _challenge(child);

        vm.warp(parent.proofDeadline());
        parent.resolve();
        child.resolve();

        assertEq(uint8(child.status()), uint8(GameStatus.CHALLENGER_WINS));
        assertEq(uint8(child.invalidationReason()), uint8(ProofLib.InvalidationReason.INVALID_PARENT));
        assertEq(child.credit(proposer), PROPOSER_BOND);
        assertEq(child.credit(challengerAccount), CHALLENGER_BOND - CHALLENGE_FEE);
        assertEq(child.credit(protocolFeeRecipient), CHALLENGE_FEE);
    }

    function test_BlacklistedParent_CascadesBeforeParentResolution() public {
        MultiProofGame parent = _proposeAtAnchor();
        MultiProofGame child = _proposeChild(0);

        vm.prank(guardian);
        asr.blacklistDisputeGame(IDisputeGame(address(parent)));
        child.resolve();

        assertEq(uint8(child.status()), uint8(GameStatus.CHALLENGER_WINS));
        assertEq(uint8(child.invalidationReason()), uint8(ProofLib.InvalidationReason.INVALID_PARENT));
    }

    function test_Cutover_AllowsRetryOfUnrespectedGame() public {
        vm.prank(guardian);
        asr.setRespectedGameType(GameType.wrap(999));
        MultiProofGame beforeCutover = _proposeAtAnchor();
        assertFalse(beforeCutover.wasRespectedGameTypeWhenCreated());
        _resolveUnchallenged(beforeCutover);

        vm.prank(guardian);
        asr.setRespectedGameType(WC_GAME_TYPE);
        _passAirgap(beforeCutover);
        beforeCutover.closeGame();
        (, uint256 anchorBlock) = asr.getAnchorRoot();
        assertEq(anchorBlock, STARTING_ANCHOR_BLOCK);

        MultiProofGame afterCutover =
            _propose(type(uint256).max, Claim.unwrap(beforeCutover.rootClaim()), beforeCutover.l2SequenceNumber(), 1);
        assertTrue(afterCutover.wasRespectedGameTypeWhenCreated());
    }

    function test_BlacklistAfterResolution_UsesRefundMode() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        _submitLanes(game, 2);
        game.resolve();

        vm.prank(guardian);
        asr.blacklistDisputeGame(IDisputeGame(address(game)));
        _passAirgap(game);
        game.closeGame();

        assertEq(uint8(game.bondDistributionMode()), uint8(BondDistributionMode.REFUND));
        assertEq(game.credit(proposer), PROPOSER_BOND);
        assertEq(game.credit(challengerAccount), CHALLENGER_BOND - CHALLENGE_FEE);
        assertEq(game.credit(protocolFeeRecipient), CHALLENGE_FEE);
        (, uint256 anchorBlock) = asr.getAnchorRoot();
        assertEq(anchorBlock, STARTING_ANCHOR_BLOCK);
    }

    function test_Retirement_UsesRefundMode() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        vm.warp(game.proofDeadline());
        game.resolve();

        vm.prank(guardian);
        asr.updateRetirementTimestamp();
        _passAirgap(game);
        game.closeGame();

        assertEq(uint8(game.bondDistributionMode()), uint8(BondDistributionMode.REFUND));
        assertEq(game.credit(proposer), PROPOSER_BOND);
        assertEq(game.credit(challengerAccount), CHALLENGER_BOND - CHALLENGE_FEE);
        assertEq(game.credit(protocolFeeRecipient), CHALLENGE_FEE);
    }

    function test_IDisputeGameSurfaceAndProofDomain() public {
        MultiProofGame game = _proposeAtAnchor();
        // Non-super game type: `rootClaimByChainId` is chain-agnostic, matching `ZKDisputeGame`.
        assertEq(Claim.unwrap(game.rootClaimByChainId(CHAIN_ID)), Claim.unwrap(game.rootClaim()));
        assertEq(Claim.unwrap(game.rootClaimByChainId(CHAIN_ID + 1)), Claim.unwrap(game.rootClaim()));

        ProofLib.Domain memory domain = game.domain();
        assertEq(domain.chainId, CHAIN_ID);
        assertEq(domain.proofSystemVersion, PROOF_SYSTEM_VERSION);
        assertEq(domain.rollupConfigHash, ROLLUP_CONFIG_HASH);
        assertEq(domain.blockInterval, BLOCK_INTERVAL);
        assertEq(game.domainHash(), ProofLib.domainHash(domain));
    }

    receive() external payable {}
}
