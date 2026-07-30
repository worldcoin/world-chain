// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {OPStackFixtures} from "./proofs/OPStackFixtures.sol";
import {MultiProofGame} from "../src/proofs/MultiProofGame.sol";
import {IMultiProofGame} from "../src/proofs/interfaces/IMultiProofGame.sol";
import {ProofLib} from "../src/proofs/lib/ProofLib.sol";

import {Vm} from "@forge-std/Vm.sol";

import {BondDistributionMode, Claim, GameStatus, GameType} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {
    GameNotOver,
    GamePaused,
    InvalidParentGame,
    NoCreditToClaim
} from "@optimism-bedrock/src/dispute/lib/Errors.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";

/// @dev A parent whose `gameData()` tries to re-enter the clone under construction.
///      `IDisputeGame.gameData()` is declared `view`, so `MultiProofGame.initialize` reaches it
///      through a STATICCALL and any state change here aborts the whole creation.
contract ReentrantParent {
    bool public reentered;

    function gameData() external returns (uint32, bytes32, bytes memory) {
        reentered = true;
        IMultiProofGame(msg.sender).resolve();
        return (1006, bytes32(0), hex"");
    }

    function status() external pure returns (uint8) {
        return uint8(GameStatus.DEFENDER_WINS);
    }

    function wasRespectedGameTypeWhenCreated() external pure returns (bool) {
        return true;
    }
}

/// @dev A view-only hostile parent: it cannot reenter, but it can impersonate a resolved
///      WIP-1006 game to probe whether registration is actually enforced.
contract ImpersonatingParent {
    function gameData() external pure returns (uint32, bytes32, bytes memory) {
        return (1006, bytes32(0), hex"");
    }

    function status() external pure returns (uint8) {
        return uint8(GameStatus.DEFENDER_WINS);
    }

    function rootClaim() external pure returns (bytes32) {
        return bytes32(0);
    }

    function l2SequenceNumber() external pure returns (uint256) {
        return 0;
    }

    function wasRespectedGameTypeWhenCreated() external pure returns (bool) {
        return true;
    }

    function domainHash() external pure returns (bytes32) {
        return bytes32(0);
    }
}

/// @dev Coverage for the WIP-1006 test-case bullets that the original suite left uncovered, plus
///      the guards added while hardening the game. Kept separate from `MultiProofGame.t.sol` so
///      the happy-path lifecycle suite stays readable.
contract MultiProofGameSafetyTest is OPStackFixtures {
    ////////////////////////////////////////////////////////////////
    //   Claim validity — the gate OptimismPortal2 withdraws on    //
    ////////////////////////////////////////////////////////////////

    // `OptimismPortal2.finalizeWithdrawalTransaction` gates on
    // `anchorStateRegistry.isGameClaimValid(game)`, and `proveWithdrawalTransaction` on
    // `isGameProper`. These assert the full accept/reject matrix for those two predicates against
    // a real `AnchorStateRegistry`, which is the entire WIP-1006 surface the Portal consumes. The
    // Portal's own output-root and storage-proof verification is stock OP code and is not
    // re-tested here.

    function test_ClaimValidity_AcceptsFinalizedRespectedGame() public {
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        IDisputeGame proxy = IDisputeGame(address(game));

        // Before the airgap the game is proper (so a withdrawal may be proven) but not yet
        // claim-valid (so it may not be finalized).
        assertTrue(asr.isGameProper(proxy));
        assertFalse(asr.isGameFinalized(proxy));
        assertFalse(asr.isGameClaimValid(proxy));

        _passAirgap(game);
        assertTrue(asr.isGameClaimValid(proxy));
    }

    function test_ClaimValidity_RejectsImmatureGame() public {
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        IDisputeGame proxy = IDisputeGame(address(game));

        // One second short of the finality delay.
        vm.warp(game.resolvedAt().raw() + FINALITY_DELAY_SECONDS);
        assertFalse(asr.isGameClaimValid(proxy));
    }

    function test_ClaimValidity_RejectsUnresolvedAndChallengerWins() public {
        MultiProofGame unresolved = _proposeAtAnchor();
        assertFalse(asr.isGameClaimValid(IDisputeGame(address(unresolved))));

        MultiProofGame timedOut = _proposeAtAnchor2();
        _challenge(timedOut);
        vm.warp(timedOut.proofDeadline());
        timedOut.resolve();
        assertEq(uint8(timedOut.status()), uint8(GameStatus.CHALLENGER_WINS));

        _passAirgap(timedOut);
        assertFalse(asr.isGameClaimValid(IDisputeGame(address(timedOut))));
    }

    function test_ClaimValidity_RejectsBlacklistedGame() public {
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);
        IDisputeGame proxy = IDisputeGame(address(game));
        assertTrue(asr.isGameClaimValid(proxy));

        vm.prank(guardian);
        asr.blacklistDisputeGame(proxy);

        assertFalse(asr.isGameProper(proxy));
        assertFalse(asr.isGameClaimValid(proxy));
    }

    function test_ClaimValidity_RejectsRetiredGame() public {
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);
        IDisputeGame proxy = IDisputeGame(address(game));
        assertTrue(asr.isGameClaimValid(proxy));

        vm.prank(guardian);
        asr.updateRetirementTimestamp();

        assertTrue(asr.isGameRetired(proxy));
        assertFalse(asr.isGameClaimValid(proxy));
    }

    function test_ClaimValidity_RejectsUnrespectedGame() public {
        vm.prank(guardian);
        asr.setRespectedGameType(GameType.wrap(999));

        MultiProofGame game = _proposeAtAnchor();
        assertFalse(game.wasRespectedGameTypeWhenCreated());
        _resolveUnchallenged(game);
        _passAirgap(game);

        IDisputeGame proxy = IDisputeGame(address(game));
        assertFalse(asr.isGameRespected(proxy));
        assertFalse(asr.isGameClaimValid(proxy));

        // Restoring the respected type does not retroactively make it claim-valid: respect is
        // captured at creation.
        vm.prank(guardian);
        asr.setRespectedGameType(WC_GAME_TYPE);
        assertFalse(asr.isGameClaimValid(proxy));
    }

    ////////////////////////////////////////////////////////////////
    //          Challenge window and single-lane finality          //
    ////////////////////////////////////////////////////////////////

    function test_Challenge_RevertsAtAndAfterDeadline() public {
        MultiProofGame game = _proposeAtAnchor();
        uint64 deadline = game.challengeDeadline();

        vm.warp(deadline);
        vm.prank(challengerAccount);
        vm.expectRevert(
            abi.encodeWithSelector(IMultiProofGame.ChallengePeriodElapsed.selector, block.timestamp, deadline)
        );
        game.challenge{value: CHALLENGER_BOND}();

        vm.warp(uint256(deadline) + 1 days);
        vm.prank(challengerAccount);
        vm.expectRevert(
            abi.encodeWithSelector(IMultiProofGame.ChallengePeriodElapsed.selector, block.timestamp, deadline)
        );
        game.challenge{value: CHALLENGER_BOND}();

        assertEq(game.challenger(), address(0));
    }

    function test_Challenge_RejectsSelfChallengeByProposer() public {
        MultiProofGame game = _proposeAtAnchor();
        stakingRegistry.setStaked(proposer, true);

        vm.prank(proposer);
        vm.expectRevert(IMultiProofGame.SelfChallenge.selector);
        game.challenge{value: CHALLENGER_BOND}();

        assertEq(game.challenger(), address(0));
    }

    function test_ChallengedSingleLane_DoesNotFinalizeBeforeProofDeadline() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);

        // Only the creation-time TEE lane counts so far.
        assertEq(game.proofCount(), 1);

        (bool resolvable, ProofLib.RootState outcome, ProofLib.InvalidationReason reason) = game.resolutionStatus();
        assertFalse(resolvable);
        assertEq(uint8(outcome), uint8(ProofLib.RootState.CHALLENGED));
        assertEq(uint8(reason), uint8(ProofLib.InvalidationReason.NONE));

        vm.expectRevert(GameNotOver.selector);
        game.resolve();

        // Still below threshold one second before the deadline.
        vm.warp(uint256(game.proofDeadline()) - 1);
        vm.expectRevert(GameNotOver.selector);
        game.resolve();
        assertEq(uint8(game.status()), uint8(GameStatus.IN_PROGRESS));
    }

    function test_Unchallenged_DoesNotResolveBeforeChallengeDeadline() public {
        MultiProofGame game = _proposeAtAnchor();

        (bool resolvable, ProofLib.RootState outcome,) = game.resolutionStatus();
        assertFalse(resolvable);
        assertEq(uint8(outcome), uint8(ProofLib.RootState.PROPOSED));

        vm.warp(uint256(game.challengeDeadline()) - 1);
        vm.expectRevert(GameNotOver.selector);
        game.resolve();

        vm.warp(game.challengeDeadline());
        (resolvable, outcome,) = game.resolutionStatus();
        assertTrue(resolvable);
        assertEq(uint8(outcome), uint8(ProofLib.RootState.FINALIZED));
    }

    function test_ResolutionStatus_ReportsProofTimeout() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        vm.warp(game.proofDeadline());

        (bool resolvable, ProofLib.RootState outcome, ProofLib.InvalidationReason reason) = game.resolutionStatus();
        assertTrue(resolvable);
        assertEq(uint8(outcome), uint8(ProofLib.RootState.INVALIDATED));
        assertEq(uint8(reason), uint8(ProofLib.InvalidationReason.PROOF_TIMEOUT));
    }

    ////////////////////////////////////////////////////////////////
    //                     L1 origin age bound                     //
    ////////////////////////////////////////////////////////////////

    function test_Create_RejectsL1OriginOlderThanMaxAge() public {
        // Move far enough forward that the fixture's origin is beyond the age bound but still
        // inside the EIP-2935 serve window, so the failure is the age bound and not availability.
        vm.roll(L1_ORIGIN_NUMBER + MAX_L1_ORIGIN_AGE + 1);

        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        vm.prank(proposer);
        vm.expectRevert(
            abi.encodeWithSelector(
                IMultiProofGame.L1OriginTooOld.selector, L1_ORIGIN_NUMBER, block.number, MAX_L1_ORIGIN_AGE
            )
        );
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), _extraData(target, type(uint256).max, 0)
        );
    }

    function test_Create_AcceptsL1OriginAtExactlyMaxAge() public {
        // Past the 256-block BLOCKHASH window, so the origin must come from EIP-2935 history.
        _seedL1History(L1_ORIGIN_NUMBER, L1_ORIGIN_HASH);
        vm.roll(L1_ORIGIN_NUMBER + MAX_L1_ORIGIN_AGE);

        MultiProofGame game = _proposeAtAnchor();
        assertEq(game.l1OriginNumber(), L1_ORIGIN_NUMBER);
    }

    function test_Create_RejectsFutureAndCurrentL1Origin() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        bytes memory extraData = _extraDataWithOrigin(target, L1_ORIGIN_HASH, block.number);

        vm.prank(proposer);
        vm.expectRevert(abi.encodeWithSelector(IMultiProofGame.InvalidL1Head.selector, L1_ORIGIN_HASH, block.number));
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), extraData);
    }

    function test_Create_ReportsHistoryUnavailableDistinctlyFromWrongHash() public {
        // Beyond the 256-block BLOCKHASH window the game consults EIP-2935. The history contract
        // is not deployed in this fixture, so the lookup cannot answer — which must surface as
        // `L1HistoryUnavailable`, not as a generic "wrong hash" rejection.
        vm.roll(L1_ORIGIN_NUMBER + 300);

        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        vm.prank(proposer);
        vm.expectRevert(abi.encodeWithSelector(IMultiProofGame.L1HistoryUnavailable.selector, L1_ORIGIN_NUMBER));
        dgf.create{value: PROPOSER_BOND}(
            WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), _extraData(target, type(uint256).max, 0)
        );
    }

    ////////////////////////////////////////////////////////////////
    //                    Configuration guards                     //
    ////////////////////////////////////////////////////////////////

    function test_Constructor_RejectsZeroBonds() public {
        IMultiProofGame.GameConfig memory config = _gameConfig();
        config.proposerBond = 0;
        vm.expectRevert(IMultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);

        config = _gameConfig();
        config.challengerBond = 0;
        vm.expectRevert(IMultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);
    }

    function test_Constructor_RejectsInvalidMaxL1OriginAge() public {
        IMultiProofGame.GameConfig memory config = _gameConfig();
        config.maxL1OriginAge = 0;
        vm.expectRevert(IMultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);

        // Beyond the EIP-2935 serve window the bound would be unreachable.
        config = _gameConfig();
        config.maxL1OriginAge = 8192;
        vm.expectRevert(IMultiProofGame.InvalidActivationParameters.selector);
        new MultiProofGame(config);
    }

    function test_Constructor_RejectsSharedLaneVerifiers() public {
        IMultiProofGame.GameConfig memory config = _gameConfig();
        config.validityProofVerifier = config.teeVerifier;
        vm.expectRevert(IMultiProofGame.DuplicateProofLaneVerifier.selector);
        new MultiProofGame(config);

        config = _gameConfig();
        config.securityCouncil = config.teeVerifier;
        vm.expectRevert(IMultiProofGame.DuplicateProofLaneVerifier.selector);
        new MultiProofGame(config);

        config = _gameConfig();
        config.securityCouncil = config.validityProofVerifier;
        vm.expectRevert(IMultiProofGame.DuplicateProofLaneVerifier.selector);
        new MultiProofGame(config);
    }

    ////////////////////////////////////////////////////////////////
    //                  Lane submission behaviour                  //
    ////////////////////////////////////////////////////////////////

    function test_SubmitProofLane_RejectsUnchallengedGame() public {
        MultiProofGame game = _proposeAtAnchor();
        bytes memory proof = abi.encodePacked(game.rootId());
        vm.expectRevert(IMultiProofGame.GameNotChallenged.selector);
        game.submitProofLane(0, proof);
    }

    function test_SubmitProofLane_RejectsUnknownLane() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        bytes memory proof = abi.encodePacked(game.rootId());
        vm.expectRevert(abi.encodeWithSelector(IMultiProofGame.InvalidLane.selector, uint8(3)));
        game.submitProofLane(3, proof);
    }

    function test_SubmitProofLane_BlockedWhilePaused() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);

        bytes memory proof = abi.encodePacked(game.rootId());

        systemConfig.setPaused(true);
        vm.expectRevert(GamePaused.selector);
        game.submitProofLane(0, proof);

        // Unpausing restores the lane, and the game still finalizes.
        systemConfig.setPaused(false);
        game.submitProofLane(0, proof);
        assertEq(game.proofCount(), 2);
    }

    function test_SubmitProofLane_SecurityCouncilLaneAcceptsAndRejects() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);

        uint8 councilLane = uint8(ProofLib.ProofLane.SECURITY_COUNCIL);
        bytes32 rootId = game.rootId();

        // Material bound to a different root is rejected as a binding mismatch.
        vm.expectRevert(
            abi.encodeWithSelector(
                IMultiProofGame.InvalidProof.selector,
                ProofLib.ProofLane.SECURITY_COUNCIL,
                rootId,
                ProofLib.VerificationStatus.BINDING_MISMATCH
            )
        );
        game.submitProofLane(councilLane, abi.encodePacked(keccak256("not-this-root")));

        game.submitProofLane(councilLane, abi.encodePacked(rootId));
        assertEq(
            game.proofBitmap(),
            ProofLib.laneMask(ProofLib.ProofLane.TEE_ATTESTATION)
                | ProofLib.laneMask(ProofLib.ProofLane.SECURITY_COUNCIL)
        );
        assertEq(game.proofCount(), 2);

        game.resolve();
        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));
    }

    function test_SubmitProofLane_SurfacesVerifierOutageDistinctly() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);

        bytes32 rootId = game.rootId();
        bytes memory proof = abi.encodePacked(rootId);

        validityVerifier.setUnavailable(true);
        vm.expectRevert(
            abi.encodeWithSelector(
                IMultiProofGame.InvalidProof.selector,
                ProofLib.ProofLane.VALIDITY_PROOF,
                rootId,
                ProofLib.VerificationStatus.UNAVAILABLE
            )
        );
        game.submitProofLane(0, proof);

        // The lane is unjudged, so recovery is a retry once the dependency returns.
        assertEq(game.proofCount(), 1);
        validityVerifier.setUnavailable(false);
        game.submitProofLane(0, proof);
        assertEq(game.proofCount(), 2);
    }

    function test_SubmitProofLane_ExpiredSubmissionLeavesBitmapUntouched() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        uint8 bitmapBefore = game.proofBitmap();
        bytes memory proof = abi.encodePacked(game.rootId());
        uint64 deadline = game.proofDeadline();

        vm.warp(deadline);
        vm.expectRevert(abi.encodeWithSelector(IMultiProofGame.ProofPeriodElapsed.selector, block.timestamp, deadline));
        game.submitProofLane(0, proof);

        assertEq(game.proofBitmap(), bitmapBefore);
        assertEq(game.proofCount(), 1);
    }

    ////////////////////////////////////////////////////////////////
    //                    Bond settlement paths                    //
    ////////////////////////////////////////////////////////////////

    function test_RefundMode_PaysBothPartiesRealEth() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        _submitLanes(game, 1);
        game.resolve();
        assertEq(uint8(game.status()), uint8(GameStatus.DEFENDER_WINS));

        vm.prank(guardian);
        asr.blacklistDisputeGame(IDisputeGame(address(game)));
        _passAirgap(game);
        game.closeGame();
        assertEq(uint8(game.bondDistributionMode()), uint8(BondDistributionMode.REFUND));

        uint256 proposerBefore = proposer.balance;
        uint256 challengerBefore = challengerAccount.balance;

        // Phase 1 unlocks both recipients, phase 2 (after the WETH delay) moves the ETH.
        game.claimCredit(proposer);
        game.claimCredit(challengerAccount);
        vm.warp(block.timestamp + WETH_DELAY_SECONDS);
        game.claimCredit(proposer);
        game.claimCredit(challengerAccount);

        assertEq(proposer.balance, proposerBefore + PROPOSER_BOND);
        assertEq(challengerAccount.balance, challengerBefore + CHALLENGER_BOND);
    }

    function test_ClaimCredit_RevertsForRecipientWithNoCredit() public {
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);
        game.closeGame();

        // Closing already happened, so the "game was open" early return does not apply and a
        // stranger with no credit gets an explicit error rather than a silent no-op.
        vm.expectRevert(NoCreditToClaim.selector);
        game.claimCredit(makeAddr("stranger"));
    }

    function test_CloseGame_IsIdempotent() public {
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);

        game.closeGame();
        assertEq(uint8(game.bondDistributionMode()), uint8(BondDistributionMode.NORMAL));

        // Must not revert: `claimCredit` calls it unconditionally.
        game.closeGame();
        assertEq(uint8(game.bondDistributionMode()), uint8(BondDistributionMode.NORMAL));
    }

    function test_Clone_AcceptsBareEthTransferWithinStipend() public {
        MultiProofGame game = _proposeAtAnchor();

        // `WETH98.withdraw` forwards only the 2300-gas stipend to this clone. If the CWIA proxy
        // ever stops short-circuiting empty calldata, this is the assertion that catches it
        // before bonds are stranded in production.
        vm.deal(address(this), 1 ether);
        (bool ok,) = address(game).call{value: 1 wei, gas: 2300}(hex"");
        assertTrue(ok);
    }

    ////////////////////////////////////////////////////////////////
    //                    Cross-language pinning                   //
    ////////////////////////////////////////////////////////////////

    function test_RootId_MatchesRustReferenceVector() public pure {
        // Mirrors `root_id_changes_when_domain_changes` in `proofs/primitives/src/types.rs`. If
        // either encoding drifts, proofs built offchain stop binding to onchain games.
        ProofLib.Domain memory domain = ProofLib.Domain({
            chainId: 4801,
            proofSystemVersion: 2,
            rollupConfigHash: bytes32(uint256(0x1111111111111111111111111111111111111111111111111111111111111111)),
            blockInterval: 10
        });
        bytes32 domainHash = ProofLib.domainHash(domain);
        assertEq(domainHash, bytes32(uint256(0xb0b28d77aae793918b08a343f71435e5abbaeceaab90a99e98f32a2563ec6386)));

        bytes32 rootId = ProofLib.rootId(
            domainHash,
            address(0x0000000000000000000000000000000000001006),
            bytes32(uint256(0x1111111111111111111111111111111111111111111111111111111111111111)),
            0,
            bytes32(uint256(0x2222222222222222222222222222222222222222222222222222222222222222)),
            10,
            bytes32(uint256(0x3333333333333333333333333333333333333333333333333333333333333333)),
            1
        );
        assertEq(rootId, bytes32(uint256(0xa6af84008c0ba6b09b25c6a1b944e6d995223057eda1a0fbc79dcff9a7ba140a)));
    }

    function test_RootId_DistinguishesPreStateForAnchorParentedGames() public view {
        // The anchor sentinel address is constant while the anchor value moves, so the pre-state
        // must be committed explicitly or two different transitions would share a rootId.
        bytes32 withAnchorA = ProofLib.rootId(
            bytes32(uint256(1)), address(asr), keccak256("anchor-a"), 100, keccak256("post"), 200, L1_ORIGIN_HASH, 1
        );
        bytes32 withAnchorB = ProofLib.rootId(
            bytes32(uint256(1)), address(asr), keccak256("anchor-b"), 100, keccak256("post"), 200, L1_ORIGIN_HASH, 1
        );
        assertTrue(withAnchorA != withAnchorB);
    }

    ////////////////////////////////////////////////////////////////
    //                           Events                            //
    ////////////////////////////////////////////////////////////////

    function test_Events_CreationEmitsGameCreatedAndFirstLane() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        bytes32 rootClaim = _rootClaimFor(target);
        bytes32 expectedRootId = ProofLib.rootId(
            ProofLib.domainHash(_domain()),
            address(asr),
            STARTING_ANCHOR_ROOT,
            STARTING_ANCHOR_BLOCK,
            rootClaim,
            target,
            L1_ORIGIN_HASH,
            L1_ORIGIN_NUMBER
        );

        vm.expectEmit(true, true, false, true);
        emit IMultiProofGame.WorldChainGameCreated(
            expectedRootId, address(asr), rootClaim, target, L1_ORIGIN_HASH, L1_ORIGIN_NUMBER, 0, proposer
        );
        vm.expectEmit(true, true, false, true);
        emit IMultiProofGame.ProofLaneSupported(
            ProofLib.ProofLane.TEE_ATTESTATION, expectedRootId, ProofLib.laneMask(ProofLib.ProofLane.TEE_ATTESTATION)
        );

        vm.prank(proposer);
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, Claim.wrap(rootClaim), _extraData(target, type(uint256).max, 0));
    }

    function test_Events_ThresholdReachedFiresExactlyOnce() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        bytes32 rootId = game.rootId();
        bytes memory proof = abi.encodePacked(rootId);

        uint8 afterValidity = ProofLib.laneMask(ProofLib.ProofLane.TEE_ATTESTATION)
            | ProofLib.laneMask(ProofLib.ProofLane.VALIDITY_PROOF);

        // Crossing the threshold emits the signal.
        vm.expectEmit(true, false, false, true);
        emit IMultiProofGame.ProofThresholdReached(rootId, afterValidity);
        game.submitProofLane(uint8(ProofLib.ProofLane.VALIDITY_PROOF), proof);

        // A third distinct lane pushes the bitmap higher but must NOT re-signal: WIP-1006 requires
        // the signal on the *first* crossing only, so consumers can treat it as an edge.
        vm.recordLogs();
        game.submitProofLane(uint8(ProofLib.ProofLane.SECURITY_COUNCIL), proof);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        for (uint256 i = 0; i < logs.length; i++) {
            assertTrue(logs[i].topics[0] != IMultiProofGame.ProofThresholdReached.selector);
        }
        assertEq(game.proofCount(), 3);
    }

    function test_Events_DuplicateLaneIsSignalledNotCounted() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        bytes32 rootId = game.rootId();

        // The TEE lane already counts from creation, so resubmitting it is a no-op.
        vm.expectEmit(true, true, false, true);
        emit IMultiProofGame.DuplicateProofLane(
            ProofLib.ProofLane.TEE_ATTESTATION, rootId, ProofLib.laneMask(ProofLib.ProofLane.TEE_ATTESTATION)
        );
        game.submitProofLane(uint8(ProofLib.ProofLane.TEE_ATTESTATION), abi.encodePacked(rootId));
        assertEq(game.proofCount(), 1);
    }

    function test_Events_ResolutionCarriesInvalidationReason() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        bytes32 rootId = game.rootId();
        vm.warp(game.proofDeadline());

        // The stock `Resolved` cannot distinguish a retryable timeout from a terminal bad parent.
        vm.expectEmit(true, false, false, true);
        emit IMultiProofGame.WorldChainResolved(
            rootId,
            GameStatus.CHALLENGER_WINS,
            ProofLib.InvalidationReason.PROOF_TIMEOUT,
            ProofLib.laneMask(ProofLib.ProofLane.TEE_ATTESTATION)
        );
        game.resolve();
    }

    function test_Events_ChallengeAndGameClosed() public {
        MultiProofGame game = _proposeAtAnchor();

        vm.expectEmit(true, false, false, true);
        emit IMultiProofGame.Challenged(challengerAccount, game.proofDeadline());
        vm.prank(challengerAccount);
        game.challenge{value: CHALLENGER_BOND}();

        _submitLanes(game, 1);
        game.resolve();
        _passAirgap(game);

        vm.expectEmit(false, false, false, true);
        emit IMultiProofGame.GameClosed(BondDistributionMode.NORMAL);
        game.closeGame();
    }

    ////////////////////////////////////////////////////////////////
    //                   Credit conservation                       //
    ////////////////////////////////////////////////////////////////

    // `resolve` mixes `+=` and `=` across four branches. Each must credit exactly `totalBonds`,
    // no more (the game would be insolvent) and no less (bonds would be stranded).

    function test_CreditConservation_UnchallengedDefenderWins() public {
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _assertCreditsSumToTotalBonds(game);
    }

    function test_CreditConservation_ChallengedDefenderWins() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        _submitLanes(game, 1);
        game.resolve();
        _assertCreditsSumToTotalBonds(game);
    }

    function test_CreditConservation_ProofTimeout() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        vm.warp(game.proofDeadline());
        game.resolve();
        _assertCreditsSumToTotalBonds(game);
    }

    function test_CreditConservation_InvalidParentRefundsBoth() public {
        MultiProofGame parent = _proposeAtAnchor();
        MultiProofGame child = _proposeChild(0);
        _challenge(child);

        _challengeAs(parent, makeAddr("other-challenger"));
        vm.warp(parent.proofDeadline());
        parent.resolve();

        child.resolve();
        assertEq(uint8(child.invalidationReason()), uint8(ProofLib.InvalidationReason.INVALID_PARENT));
        _assertCreditsSumToTotalBonds(child);
    }

    ////////////////////////////////////////////////////////////////
    //             Reentrancy during initialization                //
    ////////////////////////////////////////////////////////////////

    function test_Create_ParentCannotReenterDuringInitialize() public {
        // `initialize` reads the caller-supplied parent *before* checking factory registration,
        // and at that moment the deadlines are still zero — precisely when a reentrant `resolve()`
        // would see `block.timestamp >= challengeDeadline` and finalize the game instantly.
        //
        // What prevents it is that `IDisputeGame.gameData()` is declared `view`, so the read is a
        // STATICCALL and no state change can escape it. This test pins that: if anyone ever
        // relaxes the mutability of the parent reads, it fails.
        ReentrantParent hostileParent = new ReentrantParent();

        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        bytes memory extraData = _extraDataForParent(target, address(hostileParent), 0);

        vm.prank(proposer);
        // Bare, deliberately: a STATICCALL state-mutation abort carries no revert data.
        vm.expectRevert();
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), extraData);

        assertFalse(hostileParent.reentered(), "state change must not have survived the staticcall");
        assertEq(dgf.gameCount(), 0);
    }

    function test_Create_RejectsImpersonatingParent() public {
        // A view-only parent can fake every getter, so registration in the stock factory — not the
        // parent's self-reported data — has to be what admits it.
        ImpersonatingParent fake = new ImpersonatingParent();

        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        bytes memory extraData = _extraDataForParent(target, address(fake), 0);

        vm.prank(proposer);
        vm.expectRevert(InvalidParentGame.selector);
        dgf.create{value: PROPOSER_BOND}(WC_GAME_TYPE, Claim.wrap(_rootClaimFor(target)), extraData);
        assertEq(dgf.gameCount(), 0);
    }

    ////////////////////////////////////////////////////////////////
    //                          Helpers                            //
    ////////////////////////////////////////////////////////////////

    function _assertCreditsSumToTotalBonds(MultiProofGame game) internal view {
        uint256 total = game.totalBonds();
        assertEq(
            game.normalModeCredit(proposer) + game.normalModeCredit(game.challenger()),
            total,
            "normal-mode credits must sum to totalBonds"
        );
        assertEq(
            game.refundModeCredit(proposer) + game.refundModeCredit(game.challenger()),
            total,
            "refund-mode credits must sum to totalBonds"
        );
        assertEq(weth.balanceOf(address(game)), total, "custodied WETH must equal totalBonds");
    }

    function _challengeAs(MultiProofGame game, address account) internal {
        stakingRegistry.setStaked(account, true);
        vm.deal(account, CHALLENGER_BOND);
        vm.prank(account);
        game.challenge{value: CHALLENGER_BOND}();
    }

    /// @dev A second anchor-parented proposal with a distinct root claim, so two independent
    ///      games can coexist without colliding on the factory UUID.
    function _proposeAtAnchor2() internal returns (MultiProofGame) {
        (, uint256 anchorBlock) = asr.getAnchorRoot();
        uint256 target = anchorBlock + BLOCK_INTERVAL;
        return _propose(type(uint256).max, keccak256(abi.encode("alt-output-root", target)), target, 0);
    }

    function _extraDataWithOrigin(uint256 l2BlockNumber, bytes32 originHash, uint256 originNumber)
        internal
        view
        returns (bytes memory)
    {
        return abi.encode(
            ProofLib.domainHash(_domain()),
            l2BlockNumber,
            address(asr),
            uint256(0),
            address(0),
            originHash,
            originNumber,
            hex"01"
        );
    }
}
