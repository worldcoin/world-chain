// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {OPStackFixtures} from "./proofs/OPStackFixtures.sol";
import {WorldChainProofSystemGame} from "../src/proofs/WorldChainProofSystemGame.sol";
import {WorldChainProofLib} from "../src/proofs/WorldChainProofLib.sol";

import {BondDistributionMode, Claim, GameStatus, GameType, Hash} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {
    BadExtraData,
    ClaimAlreadyChallenged,
    GameNotFinalized,
    GamePaused,
    IncorrectBondAmount,
    InvalidParentGame,
    ParentGameNotResolved,
    UnknownChainId
} from "@optimism-bedrock/src/dispute/lib/Errors.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";

contract WorldChainProofSystemGameTest is OPStackFixtures {
    function test_Create_RegistersCanonicalGame() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
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
    }

    function test_Create_RejectsMalformedExtraData() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;

        vm.prank(proposer);
        vm.expectRevert(BadExtraData.selector);
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), abi.encode(target, address(asr))
        );

        uint256 malformedParent = uint256(uint160(address(asr))) | (uint256(1) << 160);
        bytes memory extraData =
            abi.encode(WorldChainProofLib.domainHash(_domain()), target, malformedParent, uint256(0));
        vm.prank(proposer);
        vm.expectRevert(BadExtraData.selector);
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), extraData);
    }

    function test_Create_RejectsWrongDomainAndInterval() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        bytes32 wrongDomain = keccak256("wrong-domain");

        vm.prank(proposer);
        vm.expectRevert(
            abi.encodeWithSelector(
                WorldChainProofSystemGame.InvalidDomainHash.selector, gameImpl.domainHash(), wrongDomain
            )
        );
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), abi.encode(wrongDomain, target, address(asr), uint256(0))
        );

        uint256 wrongTarget = target + 1;
        vm.prank(proposer);
        vm.expectRevert(
            abi.encodeWithSelector(WorldChainProofSystemGame.InvalidL2BlockNumber.selector, target, wrongTarget)
        );
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(wrongTarget)), _extraData(wrongTarget, type(uint256).max, 0)
        );
    }

    function test_Create_RejectsUnregisteredAndBlacklistedParents() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        vm.prank(proposer);
        vm.expectRevert(InvalidParentGame.selector);
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), _extraDataForParent(target, makeAddr("unknown-parent"), 0)
        );

        WorldChainProofSystemGame parent = _proposeAtAnchor();
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
        WorldChainProofSystemGame otherImpl = new WorldChainProofSystemGame(_gameConfig());
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

    function test_Create_UsesAnchorSentinelAfterAnchorAdvances() public {
        WorldChainProofSystemGame parent = _proposeAtAnchor();
        _resolveUnchallenged(parent);
        _passAirgap(parent);
        parent.closeGame();

        uint256 target = parent.l2SequenceNumber() + BLOCK_INTERVAL;
        bytes memory staleParentExtraData = _extraData(target, 0, 0);
        vm.prank(proposer);
        vm.expectRevert(InvalidParentGame.selector);
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), staleParentExtraData);

        WorldChainProofSystemGame child = _proposeAtAnchor();
        assertEq(child.startingRootClaim(), Claim.unwrap(parent.rootClaim()));
        assertEq(child.startingL2BlockNumber(), parent.l2SequenceNumber());
    }

    function test_Constructor_RejectsInvalidConfiguration() public {
        WorldChainProofSystemGame.GameConfig memory config = _gameConfig();
        config.proofThreshold = 0;
        vm.expectRevert(WorldChainProofSystemGame.InvalidActivationParameters.selector);
        new WorldChainProofSystemGame(config);

        config = _gameConfig();
        config.proofPeriod = config.challengePeriod;
        vm.expectRevert(WorldChainProofSystemGame.InvalidActivationParameters.selector);
        new WorldChainProofSystemGame(config);

        config = _gameConfig();
        config.domain.chainId = CHAIN_ID + 1;
        vm.expectRevert(WorldChainProofSystemGame.InconsistentSystemConfiguration.selector);
        new WorldChainProofSystemGame(config);
    }

    function test_UnchallengedFlow_AnchorsAndPaysProposer() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
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
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);
        systemConfig.setPaused(true);
        vm.expectRevert(GamePaused.selector);
        game.closeGame();
    }

    function test_Challenge_RequiresStakeBondAndOpenWindow() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        address unstaked = makeAddr("unstaked");
        vm.deal(unstaked, CHALLENGER_BOND);

        vm.prank(unstaked);
        vm.expectRevert(abi.encodeWithSelector(WorldChainProofSystemGame.UnstakedChallenger.selector, unstaked));
        game.challenge{value: CHALLENGER_BOND}();

        vm.prank(challengerAccount);
        vm.expectRevert(IncorrectBondAmount.selector);
        game.challenge{value: CHALLENGER_BOND - 1}();

        _challenge(game);
        vm.prank(challengerAccount);
        vm.expectRevert(ClaimAlreadyChallenged.selector);
        game.challenge{value: CHALLENGER_BOND}();
    }

    function test_Challenge_DoesNotExtendProofDeadline() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        uint64 proofDeadline = game.proofDeadline();
        vm.warp(game.challengeDeadline() - 1);
        _challenge(game);

        assertEq(game.proofDeadline(), proofDeadline);
        assertEq(game.challengedAt(), uint64(block.timestamp));
        assertLt(game.proofDeadline() - game.challengedAt(), game.proofPeriod());
    }

    function test_ProofThreshold_DefenderWinsAndDuplicateDoesNotCount() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _challenge(game);

        game.submitProofLane(0, abi.encodePacked(game.rootId()));
        game.submitProofLane(0, abi.encodePacked(game.rootId()));
        assertEq(game.proofCount(), 1);

        game.submitProofLane(1, abi.encodePacked(game.rootId()));
        game.resolve();
        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));
        assertEq(game.credit(proposer), PROPOSER_BOND + CHALLENGER_BOND);
    }

    function test_ProofLane_RejectsInvalidProofAndExpiredSubmission() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _challenge(game);

        vm.expectRevert();
        game.submitProofLane(0, abi.encodePacked(keccak256("wrong-root")));

        vm.warp(game.proofDeadline());
        bytes memory proof = abi.encodePacked(game.rootId());
        vm.expectRevert();
        game.submitProofLane(0, proof);
    }

    function test_ProofTimeout_ChallengerWinsAndRetryIsAllowed() public {
        WorldChainProofSystemGame first = _proposeAtAnchor();
        _challenge(first);
        vm.warp(first.proofDeadline());
        first.resolve();

        assertEq(uint8(first.status()), uint8(GameStatus.CHALLENGER_WINS));
        assertEq(uint8(first.invalidationReason()), uint8(WorldChainProofLib.InvalidationReason.PROOF_TIMEOUT));
        assertEq(first.credit(challengerAccount), PROPOSER_BOND + CHALLENGER_BOND);

        WorldChainProofSystemGame retry =
            _propose(type(uint256).max, Claim.unwrap(first.rootClaim()), first.l2SequenceNumber(), 1);
        assertEq(retry.attempt(), 1);
        assertEq(retry.startingRootClaim(), first.startingRootClaim());
    }

    function test_Retry_RejectsInProgressPreviousAttempt() public {
        WorldChainProofSystemGame first = _proposeAtAnchor();
        Claim claim = first.rootClaim();
        uint256 l2BlockNumber = first.l2SequenceNumber();
        bytes memory retryExtraData = _extraData(l2BlockNumber, type(uint256).max, 1);
        vm.prank(proposer);
        vm.expectRevert();
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, claim, retryExtraData);
    }

    function test_ChildWaitsForParentThenFinalizes() public {
        WorldChainProofSystemGame parent = _proposeAtAnchor();
        WorldChainProofSystemGame child = _proposeChild(0);
        _challenge(child);
        _submitLanes(child, 2);

        vm.expectRevert(ParentGameNotResolved.selector);
        child.resolve();

        _resolveUnchallenged(parent);
        child.resolve();
        assertEq(uint8(child.status()), uint8(GameStatus.DEFENDER_WINS));
    }

    function test_InvalidParent_CascadesAndRefundsChildBonds() public {
        WorldChainProofSystemGame parent = _proposeAtAnchor();
        _challenge(parent);
        WorldChainProofSystemGame child = _proposeChild(0);
        _challenge(child);

        vm.warp(parent.proofDeadline());
        parent.resolve();
        child.resolve();

        assertEq(uint8(child.status()), uint8(GameStatus.CHALLENGER_WINS));
        assertEq(uint8(child.invalidationReason()), uint8(WorldChainProofLib.InvalidationReason.INVALID_PARENT));
        assertEq(child.credit(proposer), PROPOSER_BOND);
        assertEq(child.credit(challengerAccount), CHALLENGER_BOND);
    }

    function test_BlacklistedParent_CascadesBeforeParentResolution() public {
        WorldChainProofSystemGame parent = _proposeAtAnchor();
        WorldChainProofSystemGame child = _proposeChild(0);

        vm.prank(guardian);
        asr.blacklistDisputeGame(IDisputeGame(address(parent)));
        child.resolve();

        assertEq(uint8(child.status()), uint8(GameStatus.CHALLENGER_WINS));
        assertEq(uint8(child.invalidationReason()), uint8(WorldChainProofLib.InvalidationReason.INVALID_PARENT));
    }

    function test_Cutover_AllowsRetryOfUnrespectedGame() public {
        vm.prank(guardian);
        asr.setRespectedGameType(GameType.wrap(999));
        WorldChainProofSystemGame beforeCutover = _proposeAtAnchor();
        assertFalse(beforeCutover.wasRespectedGameTypeWhenCreated());
        _resolveUnchallenged(beforeCutover);

        vm.prank(guardian);
        asr.setRespectedGameType(WC_GAME_TYPE);
        _passAirgap(beforeCutover);
        beforeCutover.closeGame();
        (, uint256 anchorBlock) = asr.getAnchorRoot();
        assertEq(anchorBlock, STARTING_ANCHOR_BLOCK);

        WorldChainProofSystemGame afterCutover =
            _propose(type(uint256).max, Claim.unwrap(beforeCutover.rootClaim()), beforeCutover.l2SequenceNumber(), 1);
        assertTrue(afterCutover.wasRespectedGameTypeWhenCreated());
    }

    function test_BlacklistAfterResolution_UsesRefundMode() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _challenge(game);
        _submitLanes(game, 2);
        game.resolve();

        vm.prank(guardian);
        asr.blacklistDisputeGame(IDisputeGame(address(game)));
        _passAirgap(game);
        game.closeGame();

        assertEq(uint8(game.bondDistributionMode()), uint8(BondDistributionMode.REFUND));
        assertEq(game.credit(proposer), PROPOSER_BOND);
        assertEq(game.credit(challengerAccount), CHALLENGER_BOND);
        (, uint256 anchorBlock) = asr.getAnchorRoot();
        assertEq(anchorBlock, STARTING_ANCHOR_BLOCK);
    }

    function test_Retirement_UsesRefundMode() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        _challenge(game);
        vm.warp(game.proofDeadline());
        game.resolve();

        vm.prank(guardian);
        asr.updateRetirementTimestamp();
        _passAirgap(game);
        game.closeGame();

        assertEq(uint8(game.bondDistributionMode()), uint8(BondDistributionMode.REFUND));
        assertEq(game.credit(proposer), PROPOSER_BOND);
        assertEq(game.credit(challengerAccount), CHALLENGER_BOND);
    }

    function test_IDisputeGameSurfaceAndProofDomain() public {
        WorldChainProofSystemGame game = _proposeAtAnchor();
        assertEq(Claim.unwrap(game.rootClaimByChainId(CHAIN_ID)), Claim.unwrap(game.rootClaim()));
        vm.expectRevert(UnknownChainId.selector);
        game.rootClaimByChainId(CHAIN_ID + 1);

        WorldChainProofLib.Domain memory domain = game.domain();
        assertEq(domain.chainId, CHAIN_ID);
        assertEq(domain.proofSystemVersion, PROOF_SYSTEM_VERSION);
        assertEq(domain.rollupConfigHash, ROLLUP_CONFIG_HASH);
        assertEq(domain.blockInterval, BLOCK_INTERVAL);
        assertEq(game.domainHash(), WorldChainProofLib.domainHash(domain));
    }

    receive() external payable {}
}
