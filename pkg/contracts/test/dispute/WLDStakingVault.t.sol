// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {OPStackFixtures} from "./OPStackFixtures.sol";
import {MultiProofGame} from "../../src/dispute/MultiProofGame.sol";
import {IWLDStakingVault} from "../../src/dispute/interfaces/IWLDStakingVault.sol";

import {Claim, Hash} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";

contract WLDStakingVaultAccountingTest is OPStackFixtures {
    function test_ProxyAdminIntrospectionMatchesOPProxy() public view {
        assertEq(address(bondVault.proxyAdmin()), address(proxyAdmin));
        assertEq(bondVault.proxyAdminOwner(), address(this));
    }

    function test_DepositAndDelayedWithdrawalKeepExactLiabilities() public {
        vm.prank(proposer);
        bondVault.requestWithdrawal(40 * WLD_UNIT);

        assertEq(bondVault.availableBalance(proposer), 60 * WLD_UNIT);
        assertEq(bondVault.totalLiabilities(), 200 * WLD_UNIT);
        assertTrue(bondVault.isSolvent());

        vm.warp(block.timestamp + WLD_WITHDRAWAL_DELAY_SECONDS);
        vm.prank(proposer);
        bondVault.withdraw(40 * WLD_UNIT);

        assertEq(wld.balanceOf(proposer), 40 * WLD_UNIT);
        assertEq(bondVault.totalLiabilities(), 160 * WLD_UNIT);
        assertTrue(bondVault.isSolvent());
    }

    function test_ReserveAndCreate_BindsBondProposerRatherThanFactoryCaller() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        Claim claim = Claim.wrap(_rootClaimFor(target));
        bytes memory extraData = _extraData(target, type(uint256).max, 0);

        vm.prank(proposer);
        MultiProofGame game = MultiProofGame(address(bondVault.reserveAndCreate(claim, extraData)));

        assertEq(game.gameCreator(), address(bondVault));
        assertEq(game.bondProposer(), proposer);
        assertEq(bondVault.availableBalance(proposer), 100 * WLD_UNIT - PROPOSER_BOND);
    }

    function test_Reservation_CanBeCreatedByPermissionlessKeeper() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        Claim claim = Claim.wrap(_rootClaimFor(target));
        bytes memory extraData = _extraData(target, type(uint256).max, 0);
        _reserve(proposer, claim, extraData);

        address keeper = makeAddr("permissionless-keeper");
        vm.prank(keeper);
        MultiProofGame game = MultiProofGame(address(dgf.create(WC_GAME_TYPE, claim, extraData)));

        assertEq(game.gameCreator(), keeper);
        assertEq(game.bondProposer(), proposer);
    }

    function test_Reservation_RequiresZeroFactoryEthBond() public {
        dgf.setInitBond(WC_GAME_TYPE, 1);
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;

        vm.prank(proposer);
        vm.expectRevert(abi.encodeWithSelector(IWLDStakingVault.FactoryBondMustBeZero.selector, 1));
        bondVault.reserveProposal(Claim.wrap(_rootClaimFor(target)), _extraData(target, type(uint256).max, 0));
    }

    function test_Reservation_CancelRefundsOnlyBeforeCreation() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        Claim claim = Claim.wrap(_rootClaimFor(target));
        bytes memory extraData = _extraData(target, type(uint256).max, 0);
        _reserve(proposer, claim, extraData);

        vm.prank(challengerAccount);
        vm.expectRevert(
            abi.encodeWithSelector(IWLDStakingVault.NotReservedProposer.selector, challengerAccount, proposer)
        );
        bondVault.cancelProposal(claim, extraData);

        vm.prank(proposer);
        bondVault.cancelProposal(claim, extraData);
        assertEq(bondVault.availableBalance(proposer), 100 * WLD_UNIT);
    }

    function test_Reservation_ImplReplacementMakesReservationStaleAndRefundable() public {
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        Claim claim = Claim.wrap(_rootClaimFor(target));
        bytes memory extraData = _extraData(target, type(uint256).max, 0);
        Hash uuid = dgf.getGameUUID(WC_GAME_TYPE, claim, extraData);
        _reserve(proposer, claim, extraData);

        MultiProofGame replacement = new MultiProofGame(_gameConfig());
        dgf.setImplementation(WC_GAME_TYPE, IDisputeGame(address(replacement)), hex"");

        vm.prank(creationKeeper);
        vm.expectRevert(abi.encodeWithSelector(IWLDStakingVault.StaleReservation.selector, uuid));
        dgf.create(WC_GAME_TYPE, claim, extraData);

        bondVault.invalidateStaleProposal(claim, extraData);
        assertEq(bondVault.availableBalance(proposer), 100 * WLD_UNIT);
    }

    function test_RegisteredOldImplementationCanSettleAfterReplacement() public {
        MultiProofGame game = _proposeAtAnchor();
        MultiProofGame replacement = new MultiProofGame(_gameConfig());
        dgf.setImplementation(WC_GAME_TYPE, IDisputeGame(address(replacement)), hex"");

        _resolveUnchallenged(game);
        _passAirgap(game);
        game.closeGame();

        (,, bool settled) = bondVault.gameBonds(address(game));
        assertTrue(settled);
        assertEq(bondVault.availableBalance(proposer), 100 * WLD_UNIT);
    }

    function test_WithdrawalRequestResetsDelayAndPauseOnlyBlocksTransfer() public {
        vm.prank(proposer);
        bondVault.requestWithdrawal(10 * WLD_UNIT);
        vm.warp(block.timestamp + WLD_WITHDRAWAL_DELAY_SECONDS - 1);

        vm.prank(proposer);
        bondVault.requestWithdrawal(5 * WLD_UNIT);
        (, uint256 resetAt) = bondVault.withdrawals(proposer);
        vm.warp(block.timestamp + 1);
        vm.prank(proposer);
        vm.expectRevert(
            abi.encodeWithSelector(
                IWLDStakingVault.WithdrawalDelayNotMet.selector, resetAt + WLD_WITHDRAWAL_DELAY_SECONDS
            )
        );
        bondVault.withdraw(WLD_UNIT);

        vm.warp(block.timestamp + WLD_WITHDRAWAL_DELAY_SECONDS - 1);
        systemConfig.setPaused(true);
        vm.prank(proposer);
        vm.expectRevert(IWLDStakingVault.WithdrawalPaused.selector);
        bondVault.withdraw(WLD_UNIT);

        systemConfig.setPaused(false);
        vm.prank(proposer);
        bondVault.withdraw(WLD_UNIT);
        (uint256 pending,) = bondVault.withdrawals(proposer);
        assertEq(pending, 14 * WLD_UNIT);
    }

    function test_BreakGlassHoldAndRecoverMirrorDelayedWETHTrust() public {
        bondVault.hold(proposer, 25 * WLD_UNIT);
        assertEq(bondVault.availableBalance(proposer), 75 * WLD_UNIT);
        assertEq(bondVault.availableBalance(address(this)), 25 * WLD_UNIT);
        assertTrue(bondVault.isSolvent());

        bondVault.recover(WLD_UNIT);
        assertFalse(bondVault.isSolvent());
        assertEq(bondVault.totalLiabilities(), 200 * WLD_UNIT);
    }

    function test_DirectDonationCreatesSurplusWithoutChangingLiabilities() public {
        wld.mint(address(bondVault), 5 * WLD_UNIT);

        assertEq(wld.balanceOf(address(bondVault)), 205 * WLD_UNIT);
        assertEq(bondVault.totalLiabilities(), 200 * WLD_UNIT);
        assertTrue(bondVault.isSolvent());
    }

    function test_InsolvencyDoesNotBlockInternalSettlementOrReservation() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        _submitLanes(game, PROOF_THRESHOLD);
        game.resolve();

        bondVault.recover(type(uint256).max);
        assertFalse(bondVault.isSolvent());

        _passAirgap(game);
        game.closeGame();
        assertEq(bondVault.totalLiabilities(), 200 * WLD_UNIT);

        (, uint256 anchorBlock) = asr.getAnchorRoot();
        uint256 target = anchorBlock + BLOCK_INTERVAL;
        _reserve(proposer, Claim.wrap(_rootClaimFor(target)), _extraDataForParent(target, address(game), 0));
        assertEq(bondVault.totalLiabilities(), 200 * WLD_UNIT);
    }

    function test_OverlappingParticipantRolesSettleOnceAndConserveLiabilities() public {
        MultiProofGame game = _proposeAtAnchor();
        vm.prank(proposer);
        game.challenge();
        bytes memory proof = abi.encodePacked(game.rootId());
        game.submitProofLane(_compact(0, proposer, proof));
        game.submitProofLane(_compact(1, proposer, proof));
        game.resolve();

        _passAirgap(game);
        game.closeGame();

        assertEq(bondVault.availableBalance(proposer), 100 * WLD_UNIT);
        assertEq(bondVault.totalLiabilities(), 200 * WLD_UNIT);
    }
}
