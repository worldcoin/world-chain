// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {OPStackFixtures} from "./OPStackFixtures.sol";
import {MultiProofGame} from "../../src/dispute/MultiProofGame.sol";
import {IMultiProofGame} from "../../src/dispute/interfaces/IMultiProofGame.sol";
import {IWLDStakingVault} from "../../src/dispute/interfaces/IWLDStakingVault.sol";
import {LibProof, InvalidationReason, ProofLane, TransitionPublicValues} from "../../src/dispute/lib/LibProof.sol";

import {
    BondDistributionMode,
    Claim,
    GameStatus,
    GameType,
    Hash,
    Timestamp
} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {
    BadExtraData,
    ClaimAlreadyChallenged,
    GameNotFinalized,
    GameNotOver,
    GameOver,
    GamePaused,
    InvalidParentGame,
    ParentGameNotResolved
} from "@optimism-bedrock/src/dispute/lib/Errors.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";

contract MultiProofGameTest is OPStackFixtures {
    using LibProof for uint8;

    function test_Create_RegistersCanonicalGame() public {
        MultiProofGame game = _proposeAtAnchor();
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;

        assertEq(game.gameCreator(), proposer);
        assertEq(Claim.unwrap(game.rootClaim()), _rootClaimFor(target));
        assertEq(game.l2SequenceNumber(), target);
        assertEq(game.parentRef(), address(asr));
        assertEq(Hash.unwrap(game.startingRootHash()), STARTING_ANCHOR_ROOT);
        assertEq(game.startingBlockNumber(), STARTING_ANCHOR_BLOCK);
        assertEq(game.attempt(), 0);
        assertEq(GameType.unwrap(game.gameType()), GameType.unwrap(WC_GAME_TYPE));
        assertTrue(game.wasRespectedGameTypeWhenCreated());

        (GameType gameType_, Claim rootClaim_, bytes memory extraData_) = game.gameData();
        (IDisputeGame registered,) = dgf.games(gameType_, rootClaim_, extraData_);
        assertEq(address(registered), address(game));
        assertTrue(asr.isGameRegistered(IDisputeGame(address(game))));
        (uint256 proposerBond_, uint256 challengerBond_, bool settled_) = bondVault.gameBonds(address(game));
        assertEq(proposerBond_, PROPOSER_BOND);
        assertEq(challengerBond_, 0);
        assertFalse(settled_);
        assertEq(game.proofBitmap().raw(), 0);
    }

    function test_Create_RejectsMalformedExtraData() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        Claim claim = Claim.wrap(_rootClaimFor(target));
        bytes memory shortExtraData = abi.encode(target, address(asr));

        vm.prank(proposer);
        vm.expectRevert(BadExtraData.selector);
        dgf.create(WC_GAME_TYPE, claim, shortExtraData);

        uint256 malformedParent = uint256(uint160(address(asr))) | (uint256(1) << 160);
        bytes memory extraData = abi.encode(_domainHash(), target, malformedParent, uint256(0));
        vm.prank(proposer);
        vm.expectRevert(BadExtraData.selector);
        dgf.create(WC_GAME_TYPE, claim, extraData);
    }

    function test_Create_RejectsWrongDomainAndBlockInterval() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        bytes32 wrongDomain = keccak256("wrong-domain");

        Claim claim = Claim.wrap(_rootClaimFor(target));
        bytes memory wrongDomainExtraData = abi.encode(wrongDomain, target, address(asr), uint256(0));
        vm.prank(proposer);
        vm.expectRevert(
            abi.encodeWithSelector(IMultiProofGame.InvalidDomainHash.selector, gameImpl.domainHash(), wrongDomain)
        );
        dgf.create(WC_GAME_TYPE, claim, wrongDomainExtraData);

        uint256[4] memory wrongTargets =
            [STARTING_ANCHOR_BLOCK, STARTING_ANCHOR_BLOCK - 1, STARTING_ANCHOR_BLOCK + 1, target + 1];
        for (uint256 i = 0; i < wrongTargets.length; i++) {
            Claim wrongClaim = Claim.wrap(_rootClaimFor(wrongTargets[i]));
            bytes memory wrongExtraData = _extraData(wrongTargets[i], type(uint256).max, 0);
            vm.prank(proposer);
            vm.expectRevert(
                abi.encodeWithSelector(IMultiProofGame.InvalidL2BlockNumber.selector, target, wrongTargets[i])
            );
            dgf.create(WC_GAME_TYPE, wrongClaim, wrongExtraData);
        }
    }

    function test_Create_RejectsUnregisteredAndBlacklistedParents() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        Claim claim = Claim.wrap(_rootClaimFor(target));
        bytes memory unknownParentExtraData = _extraDataForParent(target, makeAddr("unknown-parent"), 0);
        vm.prank(proposer);
        vm.expectRevert();
        dgf.create(WC_GAME_TYPE, claim, unknownParentExtraData);

        MultiProofGame parent = _proposeAtAnchor();
        vm.prank(guardian);
        asr.blacklistDisputeGame(IDisputeGame(address(parent)));

        uint256 childTarget = parent.l2SequenceNumber() + BLOCK_INTERVAL;
        bytes memory childExtraData = _extraData(childTarget, 0, 0);
        Claim childClaim = Claim.wrap(_rootClaimFor(childTarget));
        vm.prank(proposer);
        vm.expectRevert(InvalidParentGame.selector);
        dgf.create(WC_GAME_TYPE, childClaim, childExtraData);
    }

    function test_Create_UsesPreviousAnchorGameAfterAnchorAdvances() public {
        MultiProofGame parent = _proposeAtAnchor();
        _resolveUnchallenged(parent);
        _passAirgap(parent);
        parent.closeGame();

        uint256 target = parent.l2SequenceNumber() + BLOCK_INTERVAL;
        bytes memory previousAnchorParentExtraData = _extraData(target, 0, 0);
        Claim claim = Claim.wrap(_rootClaimFor(target));
        vm.prank(proposer);
        MultiProofGame previousAnchorChild =
            MultiProofGame(address(dgf.create(WC_GAME_TYPE, claim, previousAnchorParentExtraData)));
        assertEq(previousAnchorChild.parentRef(), address(parent));
        assertEq(Hash.unwrap(previousAnchorChild.startingRootHash()), Claim.unwrap(parent.rootClaim()));
        assertEq(previousAnchorChild.startingBlockNumber(), parent.l2SequenceNumber());
    }

    function test_Create_RejectsAnchorSentinelAfterAnchorAdvances() public {
        MultiProofGame parent = _proposeAtAnchor();
        _resolveUnchallenged(parent);
        _passAirgap(parent);
        parent.closeGame();
        assertEq(address(asr.anchorGame()), address(parent));

        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        Claim claim = Claim.wrap(keccak256("late-bootstrap-root"));
        bytes memory extraData = _extraData(target, type(uint256).max, 0);
        vm.prank(proposer);
        vm.expectRevert(InvalidParentGame.selector);
        dgf.create(WC_GAME_TYPE, claim, extraData);
    }

    function test_Constructor_RejectsInvalidConfiguration() public {
        IMultiProofGame.GameConfig memory config = _gameConfig();
        config.blockInterval = 0;
        vm.expectRevert(IMultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);

        config = _gameConfig();
        config.proofThreshold = 0;
        vm.expectRevert(IMultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);

        config = _gameConfig();
        config.proofThreshold = 1;
        vm.expectRevert(IMultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);

        config = _gameConfig();
        config.proofThreshold = LibProof.PROOF_LANE_COUNT + 1;
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
        config.proposerBond = 0;
        vm.expectRevert(IMultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);

        config = _gameConfig();
        config.aggregationVKey = bytes32(0);
        vm.expectRevert(IMultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);

        config = _gameConfig();
        config.rangeVKeyCommitment = bytes32(0);
        vm.expectRevert(IMultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);

        config = _gameConfig();
        config.teeImageId = bytes32(0);
        vm.expectRevert(IMultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);

        // A vault wired to a different SystemConfig than the registry's must be rejected.
        config = _gameConfig();
        vm.mockCall(address(bondVault), abi.encodeWithSignature("systemConfig()"), abi.encode(address(0xdead)));
        vm.expectRevert(IMultiProofGame.InconsistentSystemConfiguration.selector);
        new MultiProofGame(config);
        vm.clearMockedCalls();
    }

    function test_Constructor_AllowsIndependentBondAmounts() public {
        IMultiProofGame.GameConfig memory config = _gameConfig();
        config.challengerBond = CHALLENGER_BOND * 2;

        MultiProofGame implementation = new MultiProofGame(config);

        assertEq(implementation.proposerBond(), PROPOSER_BOND);
        assertEq(implementation.challengerBond(), CHALLENGER_BOND * 2);
    }

    function test_UnchallengedFlow_AnyProofLaneCanFinalize() public {
        for (uint8 lane; lane < LibProof.PROOF_LANE_COUNT; lane++) {
            (, uint256 anchorBlock) = asr.getAnchorRoot();
            uint256 target = anchorBlock + BLOCK_INTERVAL;
            bytes32 rootClaim = keccak256(abi.encode("lane", lane));
            MultiProofGame game = _propose(type(uint256).max, rootClaim, target, 0);

            game.submitProofLane(_compact(lane, laneRewardRecipient(lane), abi.encodePacked(game.rootId())));
            vm.warp(game.challengeDeadline().raw());
            game.resolve();

            assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));
            assertEq(game.normalModeCredit(proposer), PROPOSER_BOND);
        }
    }

    function test_SubmitProofLane_PassesExactGamePinnedVerifierInputs() public {
        for (uint8 lane; lane < LibProof.PROOF_LANE_COUNT; lane++) {
            (, uint256 anchorBlock) = asr.getAnchorRoot();
            MultiProofGame game =
                _propose(type(uint256).max, keccak256(abi.encode("parameters", lane)), anchorBlock + BLOCK_INTERVAL, 0);

            (Hash startingRoot, uint256 startingBlockNumber) = game.startingProposal();
            TransitionPublicValues memory transition = TransitionPublicValues({
                l1Head: Hash.unwrap(game.l1Head()),
                l2PreRoot: Hash.unwrap(startingRoot),
                // `initialize` bounds the child block to uint64, so its parent is also safe.
                // forge-lint: disable-next-line(unsafe-typecast)
                l2PreBlockNumber: uint64(startingBlockNumber),
                l2PostRoot: Claim.unwrap(game.rootClaim()),
                l2PostBlockNumber: uint64(game.l2SequenceNumber()),
                rollupConfigHash: ROLLUP_CONFIG_HASH
            });

            if (lane == uint8(ProofLane.VALIDITY_PROOF)) {
                validityVerifier.setExpectedParameters(AGGREGATION_VKEY, abi.encode(transition, RANGE_VKEY_COMMITMENT));
            } else if (lane == uint8(ProofLane.TEE_ATTESTATION)) {
                teeVerifier.setExpectedParameters(TEE_IMAGE_ID, abi.encode(transition));
            } else {
                councilVerifier.setExpectedParameters(bytes32(0), abi.encode(game.rootId()));
            }

            game.submitProofLane(_compact(lane, laneRewardRecipient(lane), abi.encodePacked(game.rootId())));
            assertTrue(game.proofBitmap().has(ProofLane(lane)));
        }
    }

    function test_UnchallengedFlow_ThresholdProvidesFastFinality() public {
        MultiProofGame game = _proposeAtAnchor();
        game.submitProofLane(_compact(0, laneRewardRecipient(0), abi.encodePacked(game.rootId())));

        (bool resolvable,,) = game.resolutionStatus();
        assertFalse(resolvable);
        vm.expectRevert(GameNotOver.selector);
        game.resolve();

        game.submitProofLane(_compact(1, laneRewardRecipient(1), abi.encodePacked(game.rootId())));
        GameStatus outcome;
        InvalidationReason reason;
        (resolvable, outcome, reason) = game.resolutionStatus();
        assertTrue(resolvable);
        assertEq(uint8(outcome), uint8(GameStatus.DEFENDER_WINS));
        assertEq(uint8(reason), uint8(InvalidationReason.NONE));

        game.resolve();
        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));
        assertEq(game.normalModeCredit(proposer), PROPOSER_BOND);
        assertLt(block.timestamp, game.challengeDeadline().raw());
    }

    function test_UnchallengedFlow_ProoflessProposalLosesBondAndCanRetry() public {
        MultiProofGame first = _proposeAtAnchor();
        vm.warp(first.challengeDeadline().raw());

        (bool resolvable, GameStatus outcome, InvalidationReason reason) = first.resolutionStatus();
        assertTrue(resolvable);
        assertEq(uint8(outcome), uint8(GameStatus.CHALLENGER_WINS));
        assertEq(uint8(reason), uint8(InvalidationReason.PROOF_TIMEOUT));

        first.resolve();
        assertEq(uint8(first.status()), uint8(GameStatus.CHALLENGER_WINS));
        assertEq(uint8(first.invalidationReason()), uint8(InvalidationReason.PROOF_TIMEOUT));
        assertEq(first.normalModeCredit(protocolFeeRecipient), PROPOSER_BOND);

        _passAirgap(first);
        first.closeGame();
        assertEq(bondVault.availableBalance(protocolFeeRecipient), PROPOSER_BOND);

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
        address keeper = makeAddr("keeper");
        vm.prank(keeper);
        game.closeGame();
        assertEq(bondVault.availableBalance(proposer), 100 * WLD_UNIT);

        (Hash anchorRoot, uint256 anchorBlock) = asr.getAnchorRoot();
        assertEq(Hash.unwrap(anchorRoot), Claim.unwrap(game.rootClaim()));
        assertEq(anchorBlock, game.l2SequenceNumber());
        assertEq(uint8(game.bondDistributionMode()), uint8(BondDistributionMode.NORMAL));

        assertEq(wld.balanceOf(proposer), 0);
        assertEq(wld.balanceOf(keeper), 0);
    }

    function test_Pause_BlocksCreationAndSettlement() public {
        systemConfig.setPaused(true);
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        Claim claim = Claim.wrap(_rootClaimFor(target));
        bytes memory extraData = _extraData(target, type(uint256).max, 0);
        vm.prank(proposer);
        vm.expectRevert(GamePaused.selector);
        dgf.create(WC_GAME_TYPE, claim, extraData);

        systemConfig.setPaused(false);
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);
        systemConfig.setPaused(true);
        vm.expectRevert(GamePaused.selector);
        game.closeGame();
    }

    function test_Challenge_RequiresBondAndOpenWindow() public {
        MultiProofGame game = _proposeAtAnchor();

        address unfundedChallenger = makeAddr("unfunded-challenger");
        vm.prank(unfundedChallenger);
        vm.expectRevert();
        game.challenge();

        _challenge(game);
        assertEq(game.proofBitmap().raw(), 0);
        assertEq(game.refundModeCredit(challengerAccount), CHALLENGER_BOND);
        (uint256 proposerBond_, uint256 challengerBond_,) = bondVault.gameBonds(address(game));
        assertEq(proposerBond_, PROPOSER_BOND);
        assertEq(challengerBond_, CHALLENGER_BOND);

        vm.prank(challengerAccount);
        vm.expectRevert(ClaimAlreadyChallenged.selector);
        game.challenge();
    }

    function test_Challenge_RejectsDivergedFactoryAndVaultOwners() public {
        MultiProofGame game = _proposeAtAnchor();
        address newFactoryOwner = makeAddr("new-factory-owner");
        dgf.transferOwnership(newFactoryOwner);

        vm.prank(challengerAccount);
        vm.expectRevert(abi.encodeWithSelector(IWLDStakingVault.OwnerMismatch.selector, newFactoryOwner, address(this)));
        game.challenge();

        assertEq(game.challenger(), address(0));
        assertEq(bondVault.availableBalance(challengerAccount), 100 * WLD_UNIT);
    }

    function test_Challenge_DoesNotExtendProofDeadline() public {
        MultiProofGame game = _proposeAtAnchor();
        uint64 challengeDeadline = game.challengeDeadline().raw();
        uint64 proofDeadline = game.proofDeadline().raw();
        vm.warp(challengeDeadline - 1);
        _challenge(game);

        assertEq(game.challengeDeadline().raw(), challengeDeadline);
        assertEq(game.proofDeadline().raw(), proofDeadline);
        (,, Timestamp deadline,,) = game.claimData();
        assertEq(deadline.raw(), proofDeadline);
        assertLt(proofDeadline - uint64(block.timestamp), game.proofPeriod().raw());
    }

    function test_ProofThreshold_DefenderWinsAndDuplicateDoesNotCount() public {
        MultiProofGame game = _proposeAtAnchor();

        bytes memory laneZero = _compact(0, laneRewardRecipient(0), abi.encodePacked(game.rootId()));
        game.submitProofLane(laneZero);

        // Hoisted: `expectRevert` binds to the next external call, so no getter may follow it.
        bytes memory expected = abi.encodeWithSelector(
            IMultiProofGame.DuplicateProofLane.selector, ProofLane.VALIDITY_PROOF, game.rootId(), game.proofBitmap()
        );
        vm.expectRevert(expected);
        game.submitProofLane(laneZero);
        assertEq(game.proofBitmap().count(), 1);

        vm.prank(challengerAccount);
        game.challenge();

        game.submitProofLane(_compact(1, laneRewardRecipient(1), abi.encodePacked(game.rootId())));
        game.resolve();
        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));

        // Two lanes plus the proposer split the forfeited challenger bond three ways.
        uint256 share = CHALLENGER_BOND / 3;
        assertEq(game.normalModeCredit(laneRewardRecipient(0)), share);
        assertEq(game.normalModeCredit(laneRewardRecipient(1)), share);
        assertEq(game.normalModeCredit(proposer), PROPOSER_BOND + CHALLENGER_BOND - 2 * share);
        assertEq(
            game.normalModeCredit(proposer) + game.normalModeCredit(laneRewardRecipient(0))
                + game.normalModeCredit(laneRewardRecipient(1)),
            game.totalBonds()
        );
    }

    function test_Challenge_AfterInitialProofStillRequiresThreshold() public {
        MultiProofGame game = _proposeAtAnchor();
        game.submitProofLane(_compact(1, laneRewardRecipient(1), abi.encodePacked(game.rootId())));
        _challenge(game);

        (bool resolvable,,) = game.resolutionStatus();
        assertFalse(resolvable);

        game.submitProofLane(_compact(0, laneRewardRecipient(0), abi.encodePacked(game.rootId())));
        game.resolve();
        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));
    }

    function test_ProofLane_RejectsInvalidProofAndExpiredSubmission() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);

        vm.expectRevert();
        game.submitProofLane(_compact(0, laneRewardRecipient(0), abi.encodePacked(keccak256("wrong-root"))));

        vm.warp(game.proofDeadline().raw());
        bytes memory proof = abi.encodePacked(game.rootId());
        vm.expectRevert();
        game.submitProofLane(_compact(0, laneRewardRecipient(0), proof));
    }

    function test_ProofLane_RejectsInitialProofAtChallengeDeadline() public {
        MultiProofGame game = _proposeAtAnchor();
        bytes memory proof = abi.encodePacked(game.rootId());
        vm.warp(game.challengeDeadline().raw());

        vm.expectRevert(GameOver.selector);
        game.submitProofLane(_compact(0, laneRewardRecipient(0), proof));
    }

    function test_ProofLane_RejectsSubmissionOnceThresholdReached() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        _submitLanes(game, PROOF_THRESHOLD);
        assertTrue(game.gameOver());

        bytes memory proof = abi.encodePacked(game.rootId());
        vm.expectRevert(GameOver.selector);
        game.submitProofLane(_compact(PROOF_THRESHOLD, laneRewardRecipient(PROOF_THRESHOLD), proof));
    }

    function test_ProofLane_RejectsSubmissionWhenParentInvalid() public {
        MultiProofGame parent = _proposeAtAnchor();
        _challenge(parent);
        MultiProofGame child = _proposeChild(0);

        vm.warp(parent.proofDeadline().raw());
        parent.resolve();

        bytes memory proof = abi.encodePacked(child.rootId());
        vm.expectRevert(InvalidParentGame.selector);
        child.submitProofLane(_compact(0, laneRewardRecipient(0), proof));
    }

    function test_Challenge_RejectsGameOver() public {
        // Once the challenge window closes.
        MultiProofGame expired = _proposeAtAnchor();
        vm.warp(expired.challengeDeadline().raw());
        vm.prank(challengerAccount);
        vm.expectRevert(GameOver.selector);
        expired.challenge();

        // Once the threshold already guarantees the defender wins.
        (, uint256 anchorBlock) = asr.getAnchorRoot();
        uint256 target = anchorBlock + BLOCK_INTERVAL;
        MultiProofGame proven = _propose(type(uint256).max, keccak256("proven-root"), target, 0);
        _submitLanes(proven, PROOF_THRESHOLD);
        vm.prank(challengerAccount);
        vm.expectRevert(GameOver.selector);
        proven.challenge();
    }

    function test_ClaimData_FollowsProposalStatusStateMachine() public {
        MultiProofGame game = _proposeAtAnchor();
        (IMultiProofGame.ProposalStatus status,,,,) = game.claimData();
        assertEq(uint8(status), uint8(IMultiProofGame.ProposalStatus.Unchallenged));

        game.submitProofLane(_compact(0, laneRewardRecipient(0), abi.encodePacked(game.rootId())));
        (status,,,,) = game.claimData();
        assertEq(uint8(status), uint8(IMultiProofGame.ProposalStatus.UnchallengedAndValidProofProvided));

        // A challenge demotes the initial proof: the lane keeps counting, but no longer
        // finalizes on its own.
        _challenge(game);
        (status,,,,) = game.claimData();
        assertEq(uint8(status), uint8(IMultiProofGame.ProposalStatus.Challenged));

        game.submitProofLane(_compact(1, laneRewardRecipient(1), abi.encodePacked(game.rootId())));
        (status,,,,) = game.claimData();
        assertEq(uint8(status), uint8(IMultiProofGame.ProposalStatus.ChallengedAndValidProofProvided));

        game.resolve();
        (status,,,,) = game.claimData();
        assertEq(uint8(status), uint8(IMultiProofGame.ProposalStatus.Resolved));
        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));
    }

    function test_ProofTimeout_ChallengerWinsAndRetryIsAllowed() public {
        MultiProofGame first = _proposeAtAnchor();
        _challenge(first);
        vm.warp(first.proofDeadline().raw());
        first.resolve();

        assertEq(uint8(first.status()), uint8(GameStatus.CHALLENGER_WINS));
        assertEq(uint8(first.invalidationReason()), uint8(InvalidationReason.PROOF_TIMEOUT));

        uint256 reward = (PROPOSER_BOND * gameImpl.CHALLENGER_REWARD_BPS()) / 10_000;
        assertEq(first.normalModeCredit(challengerAccount), CHALLENGER_BOND + reward);
        assertEq(first.normalModeCredit(protocolFeeRecipient), PROPOSER_BOND - reward);
        assertEq(
            first.normalModeCredit(challengerAccount) + first.normalModeCredit(protocolFeeRecipient),
            first.totalBonds(),
            "bonds conserved"
        );

        MultiProofGame retry = _propose(type(uint256).max, Claim.unwrap(first.rootClaim()), first.l2SequenceNumber(), 1);
        assertEq(retry.attempt(), 1);
        assertEq(Hash.unwrap(retry.startingRootHash()), Hash.unwrap(first.startingRootHash()));
    }

    function test_Retry_RejectsInProgressPreviousAttempt() public {
        MultiProofGame first = _proposeAtAnchor();
        Claim claim = first.rootClaim();
        uint256 l2BlockNumber = first.l2SequenceNumber();
        bytes memory retryExtraData = _extraData(l2BlockNumber, type(uint256).max, 1);
        vm.prank(proposer);
        vm.expectRevert();
        dgf.create(WC_GAME_TYPE, claim, retryExtraData);
    }

    function test_ChildWaitsForParentResolutionNotFinality() public {
        MultiProofGame parent = _proposeAtAnchor();
        MultiProofGame child = _proposeChild(0);
        _challenge(child);
        _submitLanes(child, 2);

        vm.expectRevert(ParentGameNotResolved.selector);
        child.resolve();

        _resolveUnchallenged(parent);
        assertFalse(asr.isGameFinalized(IDisputeGame(address(parent))));

        (bool resolvable, GameStatus outcome, InvalidationReason reason) = child.resolutionStatus();
        assertTrue(resolvable);
        assertEq(uint8(outcome), uint8(GameStatus.DEFENDER_WINS));
        assertEq(uint8(reason), uint8(InvalidationReason.NONE));

        child.resolve();
        assertEq(uint8(child.status()), uint8(GameStatus.DEFENDER_WINS));
    }

    function test_ParentBlacklistedDuringFinalityAirgap_InvalidatesChild() public {
        MultiProofGame parent = _proposeAtAnchor();
        MultiProofGame child = _proposeChild(0);
        _challenge(child);
        _submitLanes(child, 2);
        _resolveUnchallenged(parent);

        vm.prank(guardian);
        asr.blacklistDisputeGame(IDisputeGame(address(parent)));
        child.resolve();

        assertEq(uint8(child.status()), uint8(GameStatus.CHALLENGER_WINS));
        assertEq(uint8(child.invalidationReason()), uint8(InvalidationReason.INVALID_PARENT));
    }

    function test_InvalidParent_CascadesAndRefundsChildBonds() public {
        MultiProofGame parent = _proposeAtAnchor();
        _challenge(parent);
        MultiProofGame child = _proposeChild(0);
        _challenge(child);

        vm.warp(parent.proofDeadline().raw());
        parent.resolve();
        child.resolve();

        assertEq(uint8(child.status()), uint8(GameStatus.CHALLENGER_WINS));
        assertEq(uint8(child.invalidationReason()), uint8(InvalidationReason.INVALID_PARENT));
        assertEq(child.normalModeCredit(proposer), PROPOSER_BOND);
        assertEq(child.normalModeCredit(challengerAccount), CHALLENGER_BOND);
    }

    function test_BlacklistedParent_CascadesBeforeParentResolution() public {
        MultiProofGame parent = _proposeAtAnchor();
        MultiProofGame child = _proposeChild(0);

        vm.prank(guardian);
        asr.blacklistDisputeGame(IDisputeGame(address(parent)));
        child.resolve();

        assertEq(uint8(child.status()), uint8(GameStatus.CHALLENGER_WINS));
        assertEq(uint8(child.invalidationReason()), uint8(InvalidationReason.INVALID_PARENT));
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
        assertEq(game.refundModeCredit(proposer), PROPOSER_BOND);
        assertEq(game.refundModeCredit(challengerAccount), CHALLENGER_BOND);
        (, uint256 anchorBlock) = asr.getAnchorRoot();
        assertEq(anchorBlock, STARTING_ANCHOR_BLOCK);
    }

    function test_Retirement_UsesRefundMode() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        vm.warp(game.proofDeadline().raw());
        game.resolve();

        vm.prank(guardian);
        asr.updateRetirementTimestamp();
        _passAirgap(game);
        game.closeGame();

        assertEq(uint8(game.bondDistributionMode()), uint8(BondDistributionMode.REFUND));
        assertEq(game.refundModeCredit(proposer), PROPOSER_BOND);
        assertEq(game.refundModeCredit(challengerAccount), CHALLENGER_BOND);
    }

    function test_IDisputeGameSurfaceAndProofDomain() public {
        MultiProofGame game = _proposeAtAnchor();
        // Non-super game type: `rootClaimByChainId` is chain-agnostic, matching `ZKDisputeGame`.
        assertEq(Claim.unwrap(game.rootClaimByChainId(CHAIN_ID)), Claim.unwrap(game.rootClaim()));
        assertEq(Claim.unwrap(game.rootClaimByChainId(CHAIN_ID + 1)), Claim.unwrap(game.rootClaim()));

        assertEq(game.rollupConfigHash(), ROLLUP_CONFIG_HASH);
        assertEq(game.aggregationVKey(), AGGREGATION_VKEY);
        assertEq(game.rangeVKeyCommitment(), RANGE_VKEY_COMMITMENT);
        assertEq(game.teeImageId(), TEE_IMAGE_ID);
        assertEq(game.blockInterval(), BLOCK_INTERVAL);
        assertEq(game.domainHash(), _domainHash());
    }

    /// @dev A proposer cannot recover a forfeited bond by challenging from a second address.
    function test_SelfChallenge_IsAlwaysLossMaking() public {
        address sybil = makeAddr("proposer-sybil");
        _fundWLD(sybil, 10 * WLD_UNIT);

        // Challenging a proofless proposal still burns part of the proposer's bond.
        MultiProofGame proofless = _proposeAtAnchor();
        vm.prank(sybil);
        proofless.challenge();

        vm.warp(proofless.proofDeadline().raw());
        proofless.resolve();
        uint256 staked = PROPOSER_BOND + CHALLENGER_BOND;
        uint256 recovered = proofless.normalModeCredit(proposer) + proofless.normalModeCredit(sybil);
        assertLt(recovered, staked, "self-challenge must lose money");
        assertEq(staked - recovered, PROPOSER_BOND - (PROPOSER_BOND * gameImpl.CHALLENGER_REWARD_BPS()) / 10_000);

        // Proven but below threshold: self-challenging returns less than the pair staked.
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        MultiProofGame proven = _propose(type(uint256).max, keccak256("proven-self-challenge"), target, 0);
        proven.submitProofLane(
            _compact(
                uint8(ProofLane.TEE_ATTESTATION),
                laneRewardRecipient(uint8(ProofLane.TEE_ATTESTATION)),
                abi.encodePacked(proven.rootId())
            )
        );
        vm.prank(sybil);
        proven.challenge();

        vm.warp(proven.proofDeadline().raw());
        proven.resolve();

        recovered = proven.normalModeCredit(proposer) + proven.normalModeCredit(sybil);
        assertLt(recovered, staked, "self-challenge must lose money");
        assertEq(staked - recovered, PROPOSER_BOND - (PROPOSER_BOND * gameImpl.CHALLENGER_REWARD_BPS()) / 10_000);
    }

    /// @dev Unchallenged there is no forfeited stake, so the proposer still takes the whole pot.
    function test_LaneReward_UnchallengedPaysNoProvers() public {
        MultiProofGame game = _proposeAtAnchor();
        _submitLanes(game, 2);
        _resolveUnchallenged(game);

        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));
        assertEq(game.normalModeCredit(proposer), PROPOSER_BOND);
        assertEq(game.normalModeCredit(proposer), game.totalBonds());
        assertEq(game.normalModeCredit(laneRewardRecipient(0)), 0);
        assertEq(game.normalModeCredit(laneRewardRecipient(1)), 0);
    }

    /// @dev A proposer who proves every lane is settled once despite occupying multiple payout roles.
    function test_Settlement_DeduplicatesProposerAcrossProofLanes() public {
        MultiProofGame game = _proposeAtAnchor();
        bytes memory proof = abi.encodePacked(game.rootId());

        game.submitProofLane(_compact(0, proposer, proof));
        _challenge(game);
        game.submitProofLane(_compact(1, proposer, proof));
        game.resolve();

        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));
        assertEq(game.normalModeCredit(proposer), PROPOSER_BOND + CHALLENGER_BOND);
        assertEq(game.normalModeCredit(proposer), game.totalBonds());

        _passAirgap(game);
        game.closeGame();
        assertEq(bondVault.availableBalance(proposer), 100 * WLD_UNIT + CHALLENGER_BOND);
    }

    /// @dev Each lane recipient is independently credited in the shared vault.
    function test_LaneReward_RecipientsClaimIndependently() public {
        MultiProofGame game = _proposeAtAnchor();
        bytes memory proof = abi.encodePacked(game.rootId());

        game.submitProofLane(_compact(0, laneRewardRecipient(0), proof));
        _challenge(game);
        game.submitProofLane(_compact(1, laneRewardRecipient(1), proof));
        game.resolve();
        _passAirgap(game);

        uint256 share = CHALLENGER_BOND / 3;
        _claim(game, laneRewardRecipient(0));
        _claim(game, laneRewardRecipient(1));
        _claim(game, proposer);

        assertEq(wld.balanceOf(laneRewardRecipient(0)), share);
        assertEq(wld.balanceOf(laneRewardRecipient(1)), share);
        (,, bool settled) = bondVault.gameBonds(address(game));
        assertTrue(settled);
    }

    /// @dev A zero recipient forfeits its share without stranding any other payout.
    function test_LaneReward_ZeroRecipientForfeitsItsShare() public {
        MultiProofGame game = _proposeAtAnchor();
        bytes memory proof = abi.encodePacked(game.rootId());

        game.submitProofLane(_compact(0, address(0), proof));
        _challenge(game);
        game.submitProofLane(_compact(1, laneRewardRecipient(1), proof));
        game.resolve();

        uint256 share = CHALLENGER_BOND / 3;
        assertEq(game.normalModeCredit(address(0)), share);
        assertEq(game.normalModeCredit(laneRewardRecipient(1)), share);
        assertEq(game.normalModeCredit(proposer), PROPOSER_BOND + CHALLENGER_BOND - 2 * share);

        _passAirgap(game);
        game.closeGame();
        assertEq(bondVault.availableBalance(address(0)), share);
    }

    /// @dev `laneRecipient` records the first submitter; a duplicate no-op cannot reassign it.
    function test_LaneReward_DuplicateCannotStealRecipient() public {
        MultiProofGame game = _proposeAtAnchor();
        bytes memory proof = abi.encodePacked(game.rootId());
        address thief = makeAddr("lane-thief");

        game.submitProofLane(_compact(0, laneRewardRecipient(0), proof));

        vm.expectRevert(
            abi.encodeWithSelector(
                IMultiProofGame.DuplicateProofLane.selector, ProofLane.VALIDITY_PROOF, game.rootId(), game.proofBitmap()
            )
        );
        game.submitProofLane(_compact(0, thief, proof));

        assertEq(game.laneRecipient(0), laneRewardRecipient(0));
        assertEq(game.proofBitmap().count(), 1);
    }

    /// @dev A payload too short to carry a header is rejected before any lane is derived.
    function test_SubmitProofLane_RevertIf_HeaderTruncated() public {
        MultiProofGame game = _proposeAtAnchor();
        bytes memory truncated = abi.encodePacked(uint8(0), bytes19(0));
        assertEq(truncated.length, LibProof.PROOF_HEADER_LENGTH - 1);

        vm.expectRevert(
            abi.encodeWithSelector(IMultiProofGame.InvalidProof.selector, ProofLane.VALIDITY_PROOF, game.rootId())
        );
        game.submitProofLane(truncated);
    }

    /// @dev A header with no proof after it reaches the verifier and fails closed.
    function test_SubmitProofLane_RevertIf_PayloadEmpty() public {
        MultiProofGame game = _proposeAtAnchor();
        bytes memory headerOnly = _compact(0, laneRewardRecipient(0), "");
        assertEq(headerOnly.length, LibProof.PROOF_HEADER_LENGTH);

        vm.expectRevert(
            abi.encodeWithSelector(IMultiProofGame.InvalidProof.selector, ProofLane.VALIDITY_PROOF, game.rootId())
        );
        game.submitProofLane(headerOnly);
    }

    function test_FirstProofLane_DoesNotExtendChallengeWindow() public {
        MultiProofGame game = _proposeAtAnchor();
        uint64 initialDeadline = game.challengeDeadline().raw();
        assertEq(initialDeadline, game.createdAt().raw() + CHALLENGE_PERIOD);

        vm.warp(initialDeadline - 1);
        game.submitProofLane(
            _compact(
                uint8(ProofLane.TEE_ATTESTATION),
                laneRewardRecipient(uint8(ProofLane.TEE_ATTESTATION)),
                abi.encodePacked(game.rootId())
            )
        );

        assertEq(game.challengeDeadline().raw(), initialDeadline);
        vm.warp(initialDeadline);
        assertTrue(game.gameOver());

        assertEq(game.proofDeadline().raw(), game.createdAt().raw() + PROOF_PERIOD);
    }

    receive() external payable {}
}
