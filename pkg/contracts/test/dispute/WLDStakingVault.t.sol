// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {OPStackFixtures} from "./OPStackFixtures.sol";
import {MultiProofGame} from "../../src/dispute/MultiProofGame.sol";
import {WLDStakingVault} from "../../src/dispute/WLDStakingVault.sol";
import {IWLDStakingVault} from "../../src/dispute/interfaces/IWLDStakingVault.sol";
import {IDisputeGameFactory} from "@optimism-bedrock/interfaces/dispute/IDisputeGameFactory.sol";
import {GameTypes} from "../../src/dispute/lib/GameTypes.sol";

import {Claim, GameType, Hash} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {IncorrectBondAmount} from "@optimism-bedrock/src/dispute/lib/Errors.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";
import {ISystemConfig} from "@optimism-bedrock/interfaces/L1/ISystemConfig.sol";

contract UnregisteredGameHarness {
    function gameData() external pure returns (GameType, Claim, bytes memory) {
        return (GameTypes.MULTI_PROOF_GAME_TYPE, Claim.wrap(bytes32(uint256(1))), hex"");
    }
}

/// @dev Mimics a mid-creation game's surface but was not deployed by the factory, so it cannot
///      occupy the deterministic clone address the vault recomputes.
contract ImpersonatingGameHarness {
    IDisputeGameFactory internal immutable FACTORY;
    IWLDStakingVault internal immutable VAULT;
    address internal immutable VICTIM;

    constructor(IDisputeGameFactory factory, IWLDStakingVault vault, address victim) {
        FACTORY = factory;
        VAULT = vault;
        VICTIM = victim;
    }

    function gameType() external pure returns (GameType) {
        return GameTypes.MULTI_PROOF_GAME_TYPE;
    }

    function disputeGameFactory() external view returns (IDisputeGameFactory) {
        return FACTORY;
    }

    function bondVault() external view returns (IWLDStakingVault) {
        return VAULT;
    }

    function gameData() external pure returns (GameType, Claim, bytes memory) {
        return (GameTypes.MULTI_PROOF_GAME_TYPE, Claim.wrap(bytes32(uint256(1))), hex"");
    }

    function gameCreator() external view returns (address) {
        return VICTIM;
    }

    function l1Head() external pure returns (Hash) {
        return Hash.wrap(bytes32(0));
    }

    function proposerBond() external pure returns (uint256) {
        return 1e18;
    }

    function pull() external {
        VAULT.pullProposerBond();
    }
}

contract WLDStakingVaultAccountingTest is OPStackFixtures {
    function test_ProxyAdminIntrospectionMatchesOPProxy() public view {
        assertEq(address(bondVault.proxyAdmin()), address(proxyAdmin));
        assertEq(bondVault.proxyAdminOwner(), address(this));
    }

    function test_Initialize_RejectsUnauthorizedCaller() public {
        WLDStakingVault implementation = new WLDStakingVault(WLD_WITHDRAWAL_DELAY_SECONDS);
        IWLDStakingVault vault =
            IWLDStakingVault(deployCode("opstack/out/Proxy.sol/Proxy.json", abi.encode(address(proxyAdmin))));
        proxyAdmin.upgrade(payable(address(vault)), address(implementation));

        address unauthorized = makeAddr("unauthorized");
        vm.prank(unauthorized);
        vm.expectRevert(abi.encodeWithSelector(IWLDStakingVault.NotProxyAdminOwner.selector, unauthorized));
        vault.initialize(wld, ISystemConfig(address(systemConfig)), dgf);
    }

    function test_Create_PullsProposerBondFromCreatorAllowance() public {
        MultiProofGame game = _proposeAtAnchor();

        assertEq(game.gameCreator(), proposer);
        assertEq(wld.balanceOf(proposer), 100 * WLD_UNIT - PROPOSER_BOND);
        assertEq(wld.balanceOf(address(bondVault)), PROPOSER_BOND);
        assertEq(bondVault.totalLiabilities(), PROPOSER_BOND);
        assertTrue(bondVault.isSolvent());

        (uint256 proposerBond_, uint256 challengerBond_, bool settled_) = bondVault.gameBonds(address(game));
        assertEq(proposerBond_, PROPOSER_BOND);
        assertEq(challengerBond_, 0);
        assertFalse(settled_);
    }

    function test_Create_RevertsWithoutAllowance() public {
        address unapproved = makeAddr("unapproved-proposer");
        wld.mint(unapproved, PROPOSER_BOND);

        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        Claim claim = Claim.wrap(_rootClaimFor(target));
        bytes memory extraData = _extraData(target, type(uint256).max, 0);

        vm.prank(unapproved);
        vm.expectRevert();
        dgf.create(WC_GAME_TYPE, claim, extraData);
    }

    function test_Create_RejectsNonzeroFactoryEthBond() public {
        dgf.setInitBond(WC_GAME_TYPE, 1);
        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        Claim claim = Claim.wrap(_rootClaimFor(target));
        bytes memory extraData = _extraData(target, type(uint256).max, 0);

        vm.deal(proposer, 1);
        vm.prank(proposer);
        vm.expectRevert(IncorrectBondAmount.selector);
        dgf.create{value: 1}(WC_GAME_TYPE, claim, extraData);
    }

    function test_Create_RejectsDivergedFactoryAndVaultOwners() public {
        address newFactoryOwner = makeAddr("new-factory-owner");
        dgf.transferOwnership(newFactoryOwner);

        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        Claim claim = Claim.wrap(_rootClaimFor(target));
        bytes memory extraData = _extraData(target, type(uint256).max, 0);

        vm.prank(proposer);
        vm.expectRevert(abi.encodeWithSelector(IWLDStakingVault.OwnerMismatch.selector, newFactoryOwner, address(this)));
        dgf.create(WC_GAME_TYPE, claim, extraData);
    }

    function test_PullProposerBond_RejectsNonFactoryClone() public {
        ImpersonatingGameHarness impersonator = new ImpersonatingGameHarness(dgf, bondVault, proposer);

        vm.expectRevert();
        impersonator.pull();
        assertEq(wld.balanceOf(proposer), 100 * WLD_UNIT);
        assertEq(bondVault.totalLiabilities(), 0);
    }

    function test_PullChallengerBond_RejectsUnregisteredGame() public {
        UnregisteredGameHarness game = new UnregisteredGameHarness();

        vm.prank(address(game));
        vm.expectRevert(abi.encodeWithSelector(IWLDStakingVault.GameNotRegistered.selector, address(game)));
        bondVault.pullChallengerBond();
    }

    function test_Settlement_DelayedWithdrawalKeepsExactLiabilities() public {
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);
        game.closeGame();

        assertEq(bondVault.availableBalance(proposer), PROPOSER_BOND);
        assertEq(bondVault.totalLiabilities(), PROPOSER_BOND);
        assertTrue(bondVault.isSolvent());

        vm.prank(proposer);
        bondVault.requestWithdrawal(PROPOSER_BOND);
        assertEq(bondVault.availableBalance(proposer), 0);
        assertEq(bondVault.totalLiabilities(), PROPOSER_BOND);

        vm.warp(block.timestamp + WLD_WITHDRAWAL_DELAY_SECONDS);
        vm.prank(proposer);
        bondVault.withdraw(PROPOSER_BOND);

        assertEq(wld.balanceOf(proposer), 100 * WLD_UNIT);
        assertEq(bondVault.totalLiabilities(), 0);
        assertTrue(bondVault.isSolvent());
    }

    function test_WithdrawalRequestResetsDelayAndPauseOnlyBlocksTransfer() public {
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);
        game.closeGame();

        vm.prank(proposer);
        bondVault.requestWithdrawal(PROPOSER_BOND / 2);
        vm.warp(block.timestamp + WLD_WITHDRAWAL_DELAY_SECONDS - 1);

        vm.prank(proposer);
        bondVault.requestWithdrawal(PROPOSER_BOND / 4);
        (, uint256 resetAt) = bondVault.withdrawals(proposer);
        vm.warp(block.timestamp + 1);
        vm.prank(proposer);
        vm.expectRevert(
            abi.encodeWithSelector(
                IWLDStakingVault.WithdrawalDelayNotMet.selector, resetAt + WLD_WITHDRAWAL_DELAY_SECONDS
            )
        );
        bondVault.withdraw(PROPOSER_BOND / 4);

        vm.warp(block.timestamp + WLD_WITHDRAWAL_DELAY_SECONDS - 1);
        systemConfig.setPaused(true);
        vm.prank(proposer);
        vm.expectRevert(IWLDStakingVault.WithdrawalPaused.selector);
        bondVault.withdraw(PROPOSER_BOND / 4);

        systemConfig.setPaused(false);
        vm.prank(proposer);
        bondVault.withdraw(PROPOSER_BOND / 4);
        (uint256 pending,) = bondVault.withdrawals(proposer);
        assertEq(pending, PROPOSER_BOND / 2);
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
        assertEq(bondVault.availableBalance(proposer), PROPOSER_BOND);
    }

    function test_BreakGlassHoldAndRecoverMirrorDelayedWETHTrust() public {
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);
        game.closeGame();

        bondVault.hold(proposer, PROPOSER_BOND);
        assertEq(bondVault.availableBalance(proposer), 0);
        assertEq(bondVault.availableBalance(address(this)), PROPOSER_BOND);
        assertTrue(bondVault.isSolvent());

        bondVault.recover(PROPOSER_BOND);
        assertFalse(bondVault.isSolvent());
        assertEq(bondVault.totalLiabilities(), PROPOSER_BOND);
    }

    function test_BreakGlassHoldAndRecoverRejectUnauthorizedCaller() public {
        address unauthorized = makeAddr("unauthorized");

        vm.prank(unauthorized);
        vm.expectRevert(abi.encodeWithSelector(IWLDStakingVault.NotProxyAdminOwner.selector, unauthorized));
        bondVault.hold(proposer, WLD_UNIT);

        vm.prank(unauthorized);
        vm.expectRevert(abi.encodeWithSelector(IWLDStakingVault.NotProxyAdminOwner.selector, unauthorized));
        bondVault.recover(WLD_UNIT);
    }

    function test_DirectDonationCreatesSurplusWithoutChangingLiabilities() public {
        wld.mint(address(bondVault), 5 * WLD_UNIT);

        assertEq(wld.balanceOf(address(bondVault)), 5 * WLD_UNIT);
        assertEq(bondVault.totalLiabilities(), 0);
        assertTrue(bondVault.isSolvent());
    }

    function test_InsolvencyDoesNotBlockInternalSettlementOrCreation() public {
        MultiProofGame game = _proposeAtAnchor();
        _challenge(game);
        _submitLanes(game, PROOF_THRESHOLD);
        game.resolve();

        bondVault.recover(type(uint256).max);
        assertFalse(bondVault.isSolvent());

        _passAirgap(game);
        game.closeGame();
        assertEq(bondVault.totalLiabilities(), PROPOSER_BOND + CHALLENGER_BOND);

        MultiProofGame next = _proposeChild(0);
        assertEq(bondVault.totalLiabilities(), 2 * PROPOSER_BOND + CHALLENGER_BOND);
        (uint256 proposerBond_,,) = bondVault.gameBonds(address(next));
        assertEq(proposerBond_, PROPOSER_BOND);
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

        assertEq(bondVault.availableBalance(proposer), PROPOSER_BOND + CHALLENGER_BOND);
        assertEq(bondVault.totalLiabilities(), PROPOSER_BOND + CHALLENGER_BOND);
    }

    function test_Settle_RejectsUnregisteredGame() public {
        UnregisteredGameHarness game = new UnregisteredGameHarness();
        IWLDStakingVault.Payout[] memory payouts = new IWLDStakingVault.Payout[](0);

        vm.prank(address(game));
        vm.expectRevert(abi.encodeWithSelector(IWLDStakingVault.GameNotRegistered.selector, address(game)));
        bondVault.settle(payouts);
    }

    function test_Settle_RejectsInvalidPayoutTotal() public {
        MultiProofGame game = _proposeAtAnchor();
        IWLDStakingVault.Payout[] memory payouts = new IWLDStakingVault.Payout[](1);
        payouts[0] = IWLDStakingVault.Payout({recipient: proposer, amount: PROPOSER_BOND - 1});

        vm.prank(address(game));
        vm.expectRevert(
            abi.encodeWithSelector(IWLDStakingVault.InvalidPayoutTotal.selector, PROPOSER_BOND, PROPOSER_BOND - 1)
        );
        bondVault.settle(payouts);
    }

    function test_Settle_RejectsSecondSettlement() public {
        MultiProofGame game = _proposeAtAnchor();
        IWLDStakingVault.Payout[] memory payouts = new IWLDStakingVault.Payout[](1);
        payouts[0] = IWLDStakingVault.Payout({recipient: proposer, amount: PROPOSER_BOND});

        vm.prank(address(game));
        bondVault.settle(payouts);

        vm.prank(address(game));
        vm.expectRevert(abi.encodeWithSelector(IWLDStakingVault.GameAlreadySettled.selector, address(game)));
        bondVault.settle(payouts);
    }
}
