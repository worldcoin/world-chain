// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";

import {WorldChainAnchorStateRegistry} from "../src/proofs/WorldChainAnchorStateRegistry.sol";
import {WorldChainProofLib} from "../src/proofs/WorldChainProofLib.sol";
import {WorldChainProofSystemGame} from "../src/proofs/WorldChainProofSystemGame.sol";
import {WorldChainStakingRegistry} from "../src/proofs/WorldChainStakingRegistry.sol";
import {IDisputeGame} from "../src/proofs/external/IDisputeGame.sol";
import {Claim, GameStatus, GameType, Hash} from "../src/proofs/external/Types.sol";
import {MockDelayedWETH} from "../src/proofs/mocks/MockDelayedWETH.sol";
import {MockDisputeGameFactory} from "../src/proofs/mocks/MockDisputeGameFactory.sol";
import {MockRootIdVerifier} from "../src/proofs/mocks/MockRootIdVerifier.sol";

contract WorldChainProofSystemTest is Test {
    uint64 internal constant CHALLENGE_PERIOD = 1 days;
    uint64 internal constant PROOF_PERIOD = 7 days;
    uint256 internal constant PROPOSER_BOND = 1 ether;
    uint256 internal constant CHALLENGER_BOND = 0.1 ether;
    uint32 internal constant GAME_TYPE = 1006;
    uint256 internal constant WETH_DELAY = 3 days;

    uint8 internal constant LANE_VALIDITY = uint8(WorldChainProofLib.ProofLane.VALIDITY_PROOF);
    uint8 internal constant LANE_TEE = uint8(WorldChainProofLib.ProofLane.TEE_ATTESTATION);
    uint8 internal constant LANE_COUNCIL = uint8(WorldChainProofLib.ProofLane.SECURITY_COUNCIL);

    address internal proposer = address(0xA11CE);
    address internal challenger = address(0xB0B);
    address internal secondChallenger = address(0xCAFE);

    WorldChainAnchorStateRegistry internal anchor;
    MockDisputeGameFactory internal factory;
    WorldChainProofSystemGame internal gameImpl;
    MockRootIdVerifier internal validityVerifier;
    MockRootIdVerifier internal teeVerifier;
    MockRootIdVerifier internal councilVerifier;
    WorldChainStakingRegistry internal staking;
    MockDelayedWETH internal weth;

    uint256 internal proposalSalt;

    function setUp() public {
        vm.deal(proposer, 100 ether);
        vm.deal(challenger, 100 ether);
        vm.deal(secondChallenger, 100 ether);

        // Roll forward so blockhash(block.number - 1) is non-zero in the factory.
        vm.roll(100);

        anchor = new WorldChainAnchorStateRegistry(bytes32(uint256(1)), 0, GameType.wrap(GAME_TYPE));
        staking = new WorldChainStakingRegistry(CHALLENGER_BOND);
        staking.setStaked(challenger, true);
        staking.setStaked(secondChallenger, true);

        validityVerifier = new MockRootIdVerifier(false);
        teeVerifier = new MockRootIdVerifier(false);
        councilVerifier = new MockRootIdVerifier(false);
        weth = new MockDelayedWETH(WETH_DELAY);

        gameImpl = new WorldChainProofSystemGame(
            GameType.wrap(GAME_TYPE),
            WorldChainProofLib.Domain({
                chainId: 4801,
                proofSystemVersion: 1,
                rollupConfigHash: keccak256("world-chain-devnet-rollup-config"),
                blockInterval: 0,
                intermediateBlockInterval: 0
            }),
            CHALLENGE_PERIOD,
            PROOF_PERIOD,
            validityVerifier,
            teeVerifier,
            councilVerifier,
            staking,
            weth
        );

        factory = new MockDisputeGameFactory();
        factory.setImplementation(GameType.wrap(GAME_TYPE), IDisputeGame(address(gameImpl)));
        factory.setInitBond(GameType.wrap(GAME_TYPE), PROPOSER_BOND);
    }

    // --------------------------- Test Case 1 --------------------------- //
    // An unchallenged root finalizes after exactly CHALLENGE_PERIOD.
    function testUnchallengedRootFinalizesAfterChallengePeriod() public {
        WorldChainProofSystemGame game = _propose(10);

        // Just before the deadline: resolve reverts.
        vm.warp(block.timestamp + CHALLENGE_PERIOD - 1);
        vm.expectRevert();
        game.resolve();

        vm.warp(block.timestamp + 1);
        game.resolve();

        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));
        assertEq(game.proofCount(), 0);
    }

    // --------------------------- Test Case 2 --------------------------- //
    // An unstaked account cannot challenge.
    function testUnstakedAccountCannotChallenge() public {
        WorldChainProofSystemGame game = _propose(10);

        vm.deal(address(0xBAD), 1 ether);
        vm.prank(address(0xBAD));
        vm.expectRevert();
        game.challenge{value: CHALLENGER_BOND}();
    }

    // --------------------------- Test Case 3 --------------------------- //
    // A staked account can challenge without proof before the deadline.
    function testStakedAccountCanChallengeWithoutProofBeforeDeadline() public {
        WorldChainProofSystemGame game = _propose(10);

        _challenge(game, challenger);

        assertEq(uint8(game.status()), uint8(GameStatus.IN_PROGRESS));
        assertTrue(game.challenged());
        assertEq(game.proofDeadline(), block.timestamp + PROOF_PERIOD);
        assertEq(game.proofCount(), 0);
    }

    // --------------------------- Test Case 4 --------------------------- //
    // A challenge at or after the deadline reverts.
    function testChallengeAtOrAfterDeadlineReverts() public {
        WorldChainProofSystemGame game = _propose(10);

        vm.warp(block.timestamp + CHALLENGE_PERIOD);
        vm.prank(challenger);
        vm.expectRevert();
        game.challenge{value: CHALLENGER_BOND}();
    }

    // --------------------------- Test Case 5 --------------------------- //
    // A challenged root with one supporting lane does not finalize.
    function testOneSupportingLaneDoesNotFinalize() public {
        (WorldChainProofSystemGame game, bytes32 rootId) = _proposeAndChallenge(10);

        game.submitProofLane(LANE_VALIDITY, abi.encode(rootId));

        assertEq(game.proofCount(), 1);
        assertEq(uint8(game.status()), uint8(GameStatus.IN_PROGRESS));

        vm.expectRevert();
        game.resolve();
    }

    // --------------------------- Test Case 6 --------------------------- //
    // Proposer defends a challenged root to finalization with two distinct lanes.
    function testProposerDefendsWithTwoDistinctLanes() public {
        (WorldChainProofSystemGame game, bytes32 rootId) = _proposeAndChallenge(10);

        game.submitProofLane(LANE_VALIDITY, abi.encode(rootId));
        game.submitProofLane(LANE_TEE, abi.encode(rootId));

        assertEq(game.proofCount(), 2);
        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));
    }

    // --------------------------- Test Case 7 --------------------------- //
    // A challenged root not defended to threshold by proofDeadline invalidates
    // and forfeits PROPOSER_BOND.
    function testChallengedRootInvalidatesAndForfeitsProposerBond() public {
        (WorldChainProofSystemGame game, bytes32 rootId) = _proposeAndChallenge(10);
        game.submitProofLane(LANE_VALIDITY, abi.encode(rootId));

        // Cannot invalidate while the proof window is open.
        vm.expectRevert();
        game.invalidate();

        vm.warp(block.timestamp + PROOF_PERIOD);
        game.invalidate();

        assertEq(uint8(game.status()), uint8(GameStatus.CHALLENGER_WINS));

        // Proposer bond is forfeited to the successful challenger.
        uint256 challengerBefore = challenger.balance;
        _claimAfterDelay(game);
        assertEq(challenger.balance, challengerBefore + PROPOSER_BOND);
    }

    // --------------------------- Test Case 8 --------------------------- //
    // A lane submission at or after proofDeadline reverts and does not affect
    // the proof bitmap.
    function testLaneSubmissionAtProofDeadlineReverts() public {
        (WorldChainProofSystemGame game, bytes32 rootId) = _proposeAndChallenge(10);

        vm.warp(block.timestamp + PROOF_PERIOD);
        vm.expectRevert();
        game.submitProofLane(LANE_VALIDITY, abi.encode(rootId));

        assertEq(game.proofBitmap(), 0);
    }

    // --------------------------- Test Case 9 --------------------------- //
    // Subsequent challenges on an already-challenged root do not extend
    // proofDeadline.
    function testSubsequentChallengeDoesNotExtendProofDeadline() public {
        (WorldChainProofSystemGame game,) = _proposeAndChallenge(10);
        uint64 firstDeadline = game.proofDeadline();

        vm.warp(block.timestamp + 1 hours);
        _challenge(game, secondChallenger);

        assertEq(game.proofDeadline(), firstDeadline);
    }

    // --------------------------- Test Case 10 -------------------------- //
    // Duplicate submissions from the same lane do not increase the threshold
    // count.
    function testDuplicateLaneDoesNotIncreaseThresholdCount() public {
        (WorldChainProofSystemGame game, bytes32 rootId) = _proposeAndChallenge(10);

        game.submitProofLane(LANE_VALIDITY, abi.encode(rootId));
        game.submitProofLane(LANE_VALIDITY, abi.encode(rootId));

        assertEq(game.proofCount(), 1);
        assertEq(uint8(game.status()), uint8(GameStatus.IN_PROGRESS));
    }

    // --------------------------- Test Case 11 -------------------------- //
    // Every proof lane rejects material that does not bind to rootId.
    function testEveryLaneRejectsMaterialForDifferentRootId() public {
        (WorldChainProofSystemGame game,) = _proposeAndChallenge(10);
        bytes memory wrong = abi.encode(bytes32(uint256(0x1234)));

        vm.expectRevert();
        game.submitProofLane(LANE_VALIDITY, wrong);
        vm.expectRevert();
        game.submitProofLane(LANE_TEE, wrong);
        vm.expectRevert();
        game.submitProofLane(LANE_COUNCIL, wrong);

        assertEq(game.proofBitmap(), 0);
    }

    // --------------------------- Test Case 12 -------------------------- //
    // Anchor updates reject non-finalized, invalidated, paused, retired, or
    // non-monotonic roots.
    function testAnchorUpdatesRejectInvalidRoots() public {
        // Non-finalized.
        WorldChainProofSystemGame nonFinalized = _propose(10);
        vm.expectRevert();
        anchor.setAnchorState(IDisputeGame(address(nonFinalized)));

        // Invalidated.
        (WorldChainProofSystemGame invalidated, bytes32 rootId) = _proposeAndChallenge(11);
        invalidated.submitProofLane(LANE_VALIDITY, abi.encode(rootId));
        vm.warp(block.timestamp + PROOF_PERIOD);
        invalidated.invalidate();
        vm.expectRevert();
        anchor.setAnchorState(IDisputeGame(address(invalidated)));

        // Paused.
        WorldChainProofSystemGame finalized = _finalizedGame(12);
        anchor.setPaused(true);
        vm.expectRevert();
        anchor.setAnchorState(IDisputeGame(address(finalized)));
        anchor.setPaused(false);

        // Retired.
        anchor.setRetired(true);
        vm.expectRevert();
        anchor.setAnchorState(IDisputeGame(address(finalized)));
        anchor.setRetired(false);

        // Accepted once.
        anchor.setAnchorState(IDisputeGame(address(finalized)));
        assertEq(anchor.currentL2BlockNumber(), 12);
        assertEq(anchor.anchorGame(), address(finalized));

        // Non-monotonic (lower block number).
        WorldChainProofSystemGame lower = _finalizedGame(5);
        vm.expectRevert();
        anchor.setAnchorState(IDisputeGame(address(lower)));
    }

    // ----------------------- IDisputeGame conformance ------------------ //

    function testIDisputeGameConformance() public {
        bytes32 rootClaim = keccak256("conformance-root");
        bytes memory extra = _extraData(99, address(anchor));

        vm.prank(proposer);
        IDisputeGame game = factory.create{value: PROPOSER_BOND}(GameType.wrap(GAME_TYPE), Claim.wrap(rootClaim), extra);

        // CWIA readers match the create() args.
        assertEq(game.gameCreator(), proposer);
        assertEq(Claim.unwrap(game.rootClaim()), rootClaim);
        assertEq(Hash.unwrap(game.l1Head()), blockhash(block.number - 1));
        assertEq(game.l2BlockNumber(), 99);
        assertEq(keccak256(game.extraData()), keccak256(extra));
        assertEq(GameType.unwrap(game.gameType()), GAME_TYPE);

        (GameType gt, Claim rc, bytes memory ed) = game.gameData();
        assertEq(GameType.unwrap(gt), GAME_TYPE);
        assertEq(Claim.unwrap(rc), rootClaim);
        assertEq(keccak256(ed), keccak256(extra));

        assertEq(uint8(game.status()), uint8(GameStatus.IN_PROGRESS));
        assertTrue(game.createdAt().raw() > 0);

        // parentRef parsed from extraData.
        assertEq(WorldChainProofSystemGame(payable(address(game))).parentRef(), address(anchor));

        // Factory indexes by UUID.
        (IDisputeGame stored,) = factory.games(GameType.wrap(GAME_TYPE), Claim.wrap(rootClaim), extra);
        assertEq(address(stored), address(game));
    }

    function testFactoryRejectsDuplicateUUID() public {
        bytes32 rootClaim = keccak256("dup");
        bytes memory extra = _extraData(20, address(anchor));

        vm.prank(proposer);
        factory.create{value: PROPOSER_BOND}(GameType.wrap(GAME_TYPE), Claim.wrap(rootClaim), extra);

        vm.prank(proposer);
        vm.expectRevert();
        factory.create{value: PROPOSER_BOND}(GameType.wrap(GAME_TYPE), Claim.wrap(rootClaim), extra);
    }

    function testFactoryRejectsWrongBond() public {
        bytes memory extra = _extraData(20, address(anchor));
        vm.prank(proposer);
        vm.expectRevert();
        factory.create{value: PROPOSER_BOND - 1}(GameType.wrap(GAME_TYPE), Claim.wrap(keccak256("x")), extra);
    }

    // ------------------------- DelayedWETH payout ---------------------- //

    function testDefenderWinsBondReturnedToProposerViaDelayedWeth() public {
        WorldChainProofSystemGame game = _finalizedGame(10);

        // Bond escrowed in DelayedWETH against the game.
        assertEq(weth.balanceOf(address(game)), PROPOSER_BOND);

        // Withdrawal cannot complete before the delay.
        vm.expectRevert();
        game.claimCredit();

        uint256 before = proposer.balance;
        vm.warp(block.timestamp + WETH_DELAY);
        game.claimCredit();
        assertEq(proposer.balance, before + PROPOSER_BOND);

        // Second claim is a no-op.
        game.claimCredit();
        assertEq(proposer.balance, before + PROPOSER_BOND);
    }

    function testClaimCreditRevertsWhileInProgress() public {
        WorldChainProofSystemGame game = _propose(10);
        vm.expectRevert();
        game.claimCredit();
    }

    // ---------------------- Challenger forfeit/refund ------------------ //

    function testChallengerBondRefundedOnInvalidation() public {
        (WorldChainProofSystemGame game,) = _proposeAndChallenge(10);
        assertEq(staking.lockedBonds(address(game), challenger), CHALLENGER_BOND);

        uint256 before = challenger.balance;
        vm.warp(block.timestamp + PROOF_PERIOD);
        game.invalidate();

        // Challenger bond refunded immediately on invalidation.
        assertEq(challenger.balance, before + CHALLENGER_BOND);
        assertEq(staking.lockedBonds(address(game), challenger), 0);
    }

    function testChallengerBondForfeitedOnFinalization() public {
        WorldChainProofSystemGame game = _finalizedGame(10);

        // Bond forfeited to the registry; not refunded.
        assertEq(staking.lockedBonds(address(game), challenger), 0);
        assertEq(staking.forfeitedBalance(), CHALLENGER_BOND);
    }

    // --------------------------- Azul extraData ------------------------ //

    function testIntermediateRootsExtraDataValidation() public {
        // A game impl with a 2:1 block interval ratio expects 2 intermediate
        // roots, the last of which must equal rootClaim.
        WorldChainProofSystemGame intermediateImpl = new WorldChainProofSystemGame(
            GameType.wrap(GAME_TYPE),
            WorldChainProofLib.Domain({
                chainId: 4801,
                proofSystemVersion: 1,
                rollupConfigHash: keccak256("ratio"),
                blockInterval: 10,
                intermediateBlockInterval: 5
            }),
            CHALLENGE_PERIOD,
            PROOF_PERIOD,
            validityVerifier,
            teeVerifier,
            councilVerifier,
            staking,
            weth
        );
        MockDisputeGameFactory ratioFactory = new MockDisputeGameFactory();
        ratioFactory.setImplementation(GameType.wrap(GAME_TYPE), IDisputeGame(address(intermediateImpl)));
        ratioFactory.setInitBond(GameType.wrap(GAME_TYPE), PROPOSER_BOND);

        bytes32 rootClaim = keccak256("final-root");
        bytes32 firstRoot = keccak256("first-root");

        // Final intermediate root must equal rootClaim.
        bytes memory good = abi.encodePacked(uint256(10), address(anchor), firstRoot, rootClaim);
        vm.prank(proposer);
        ratioFactory.create{value: PROPOSER_BOND}(GameType.wrap(GAME_TYPE), Claim.wrap(rootClaim), good);

        // Wrong final root reverts during initialize.
        bytes memory bad = abi.encodePacked(uint256(11), address(anchor), firstRoot, keccak256("nope"));
        vm.prank(proposer);
        vm.expectRevert();
        ratioFactory.create{value: PROPOSER_BOND}(GameType.wrap(GAME_TYPE), Claim.wrap(rootClaim), bad);
    }

    // ------------------------------- Helpers --------------------------- //

    function _extraData(uint256 l2BlockNumber, address parentRef) internal pure returns (bytes memory) {
        return abi.encodePacked(l2BlockNumber, parentRef);
    }

    function _propose(uint256 l2BlockNumber) internal returns (WorldChainProofSystemGame game) {
        proposalSalt++;
        bytes32 rootClaim = keccak256(abi.encode("root", l2BlockNumber, proposalSalt));
        bytes memory extra = _extraData(l2BlockNumber, address(anchor));
        vm.prank(proposer);
        IDisputeGame created =
            factory.create{value: PROPOSER_BOND}(GameType.wrap(GAME_TYPE), Claim.wrap(rootClaim), extra);
        return WorldChainProofSystemGame(payable(address(created)));
    }

    function _proposeAndChallenge(uint256 l2BlockNumber)
        internal
        returns (WorldChainProofSystemGame game, bytes32 rootId)
    {
        game = _propose(l2BlockNumber);
        _challenge(game, challenger);
        rootId = game.rootId();
    }

    function _finalizedGame(uint256 l2BlockNumber) internal returns (WorldChainProofSystemGame game) {
        bytes32 rootId;
        (game, rootId) = _proposeAndChallenge(l2BlockNumber);
        game.submitProofLane(LANE_VALIDITY, abi.encode(rootId));
        game.submitProofLane(LANE_TEE, abi.encode(rootId));
    }

    function _challenge(WorldChainProofSystemGame game, address account) internal {
        vm.prank(account);
        game.challenge{value: CHALLENGER_BOND}();
    }

    function _claimAfterDelay(WorldChainProofSystemGame game) internal {
        vm.warp(block.timestamp + WETH_DELAY);
        game.claimCredit();
    }
}
