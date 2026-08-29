// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {OPStackFixtures} from "./OPStackFixtures.sol";
import {MultiProofGame} from "../../src/dispute/MultiProofGame.sol";
import {ERC20StakingVault} from "../../src/dispute/ERC20StakingVault.sol";
import {IERC20StakingVault} from "../../src/dispute/interfaces/IERC20StakingVault.sol";
import {
    GameAlreadySettled,
    GameNotRegistered,
    InsufficientBalance,
    InvalidAccount,
    InvalidAmount,
    InvalidPayoutTotal,
    NotProxyAdminOwner,
    OwnerMismatch,
    WithdrawalDelayNotMet,
    WithdrawalPaused
} from "../../src/dispute/lib/Errors.sol";
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
    IERC20StakingVault internal immutable VAULT;
    address internal immutable VICTIM;

    constructor(IDisputeGameFactory factory, IERC20StakingVault vault, address victim) {
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

    function bondVault() external view returns (IERC20StakingVault) {
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

    function lock() external {
        VAULT.lockProposerBond();
    }
}

contract ERC20StakingVaultAccountingTest is OPStackFixtures {
    function test_ProxyAdminIntrospectionMatchesOPProxy() public view {
        assertEq(address(bondVault.proxyAdmin()), address(proxyAdmin));
        assertEq(bondVault.proxyAdminOwner(), address(this));
    }

    function test_Initialize_RejectsUnauthorizedCaller() public {
        ERC20StakingVault implementation = new ERC20StakingVault(ERC20_WITHDRAWAL_DELAY_SECONDS);
        IERC20StakingVault vault =
            IERC20StakingVault(deployCode("opstack/out/Proxy.sol/Proxy.json", abi.encode(address(proxyAdmin))));
        proxyAdmin.upgrade(payable(address(vault)), address(implementation));

        address unauthorized = makeAddr("unauthorized");
        vm.prank(unauthorized);
        vm.expectRevert(abi.encodeWithSelector(NotProxyAdminOwner.selector, unauthorized));
        vault.initialize(bondToken, ISystemConfig(address(systemConfig)), dgf);
    }

    function test_DepositCreditsSelectedAvailableBalances() public {
        address funder = makeAddr("funder");
        address beneficiary = makeAddr("beneficiary");
        uint256 amount = 3 * TOKEN_UNIT;
        bondToken.mint(funder, amount);

        vm.prank(funder);
        bondToken.approve(address(bondVault), amount);
        vm.prank(funder);
        bondVault.deposit(funder, TOKEN_UNIT);
        vm.prank(funder);
        bondVault.deposit(beneficiary, 2 * TOKEN_UNIT);

        assertEq(bondVault.availableBalance(funder), TOKEN_UNIT);
        assertEq(bondVault.availableBalance(beneficiary), 2 * TOKEN_UNIT);
        assertEq(bondToken.allowance(funder, address(bondVault)), 0);
        assertEq(bondToken.balanceOf(funder), 0);
        assertEq(bondToken.balanceOf(address(bondVault)), 203 * TOKEN_UNIT);
    }

    function test_DepositRejectsZeroAccount() public {
        vm.prank(proposer);
        vm.expectRevert(InvalidAccount.selector);
        bondVault.deposit(address(0), TOKEN_UNIT);
    }

    function test_DepositRejectsZeroAmount() public {
        vm.prank(proposer);
        vm.expectRevert(InvalidAmount.selector);
        bondVault.deposit(proposer, 0);
    }

    function test_PauseAllowsDepositsAndWithdrawalRequests() public {
        address funder = makeAddr("paused-funder");
        address beneficiary = makeAddr("paused-beneficiary");
        bondToken.mint(funder, TOKEN_UNIT);
        vm.prank(funder);
        bondToken.approve(address(bondVault), TOKEN_UNIT);
        systemConfig.setPaused(true);

        vm.prank(funder);
        bondVault.deposit(beneficiary, TOKEN_UNIT);
        vm.prank(proposer);
        bondVault.requestWithdrawal(TOKEN_UNIT);

        assertEq(bondVault.availableBalance(beneficiary), TOKEN_UNIT);
        assertEq(bondVault.availableBalance(proposer), 99 * TOKEN_UNIT);
        (uint256 pending,) = bondVault.withdrawals(proposer);
        assertEq(pending, TOKEN_UNIT);
    }

    function test_Create_LocksProposerBondFromAvailableBalance() public {
        MultiProofGame game = _proposeAtAnchor();

        assertEq(game.gameCreator(), proposer);
        assertEq(bondVault.availableBalance(proposer), 99 * TOKEN_UNIT);
        assertEq(bondToken.balanceOf(proposer), 0);
        assertEq(bondToken.allowance(proposer, address(bondVault)), 0);
        assertEq(bondToken.balanceOf(address(bondVault)), 200 * TOKEN_UNIT);

        (uint256 proposerBond_, uint256 challengerBond_, bool settled_) = bondVault.gameBonds(address(game));
        assertEq(proposerBond_, PROPOSER_BOND);
        assertEq(challengerBond_, 0);
        assertFalse(settled_);
    }

    function test_Create_RevertsWithoutAvailableBalance() public {
        address unfunded = makeAddr("unfunded-proposer");
        bondToken.mint(unfunded, PROPOSER_BOND);
        vm.prank(unfunded);
        bondToken.approve(address(bondVault), PROPOSER_BOND);

        uint256 target = STARTING_ANCHOR_BLOCK + BLOCK_INTERVAL;
        Claim claim = Claim.wrap(_rootClaimFor(target));
        bytes memory extraData = _extraData(target, type(uint256).max, 0);

        vm.prank(unfunded);
        vm.expectRevert(abi.encodeWithSelector(InsufficientBalance.selector, unfunded, 0, PROPOSER_BOND));
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
        vm.expectRevert(abi.encodeWithSelector(OwnerMismatch.selector, newFactoryOwner, address(this)));
        dgf.create(WC_GAME_TYPE, claim, extraData);
    }

    function test_LockProposerBond_RejectsNonFactoryClone() public {
        ImpersonatingGameHarness impersonator = new ImpersonatingGameHarness(dgf, bondVault, proposer);

        vm.expectRevert();
        impersonator.lock();
        assertEq(bondVault.availableBalance(proposer), 100 * TOKEN_UNIT);
    }

    function test_LockChallengerBond_RejectsUnregisteredGame() public {
        UnregisteredGameHarness game = new UnregisteredGameHarness();

        vm.prank(address(game));
        vm.expectRevert(abi.encodeWithSelector(GameNotRegistered.selector, address(game)));
        bondVault.lockChallengerBond();
    }

    function test_Settlement_DelayedWithdrawalTransfersBondToken() public {
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);
        game.closeGame();

        assertEq(bondVault.availableBalance(proposer), 100 * TOKEN_UNIT);

        vm.prank(proposer);
        bondVault.requestWithdrawal(PROPOSER_BOND);
        assertEq(bondVault.availableBalance(proposer), 99 * TOKEN_UNIT);

        vm.warp(block.timestamp + ERC20_WITHDRAWAL_DELAY_SECONDS);
        vm.prank(proposer);
        bondVault.withdraw(PROPOSER_BOND);

        assertEq(bondToken.balanceOf(proposer), PROPOSER_BOND);
        assertEq(bondVault.availableBalance(proposer), 99 * TOKEN_UNIT);
        assertEq(bondToken.balanceOf(address(bondVault)), 199 * TOKEN_UNIT);
    }

    function test_WithdrawalRequestResetsDelayAndPauseOnlyBlocksTransfer() public {
        MultiProofGame game = _proposeAtAnchor();
        _resolveUnchallenged(game);
        _passAirgap(game);
        game.closeGame();

        vm.prank(proposer);
        bondVault.requestWithdrawal(PROPOSER_BOND / 2);
        vm.warp(block.timestamp + ERC20_WITHDRAWAL_DELAY_SECONDS - 1);

        vm.prank(proposer);
        bondVault.requestWithdrawal(PROPOSER_BOND / 4);
        (, uint256 resetAt) = bondVault.withdrawals(proposer);
        vm.warp(block.timestamp + 1);
        vm.prank(proposer);
        vm.expectRevert(
            abi.encodeWithSelector(WithdrawalDelayNotMet.selector, resetAt + ERC20_WITHDRAWAL_DELAY_SECONDS)
        );
        bondVault.withdraw(PROPOSER_BOND / 4);

        vm.warp(block.timestamp + ERC20_WITHDRAWAL_DELAY_SECONDS - 1);
        systemConfig.setPaused(true);
        vm.prank(proposer);
        vm.expectRevert(WithdrawalPaused.selector);
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
        assertEq(bondVault.availableBalance(proposer), 100 * TOKEN_UNIT);
    }

    function test_SettledBondIsImmediatelyReusableForNextProposal() public {
        MultiProofGame first = _proposeAtAnchor();
        _resolveUnchallenged(first);
        _passAirgap(first);
        first.closeGame();

        MultiProofGame second = _proposeChild(0);

        assertEq(bondVault.availableBalance(proposer), 99 * TOKEN_UNIT);
        assertEq(bondToken.balanceOf(proposer), 0);
        assertEq(bondToken.allowance(proposer, address(bondVault)), 0);
        (uint256 proposerBond_,,) = bondVault.gameBonds(address(second));
        assertEq(proposerBond_, PROPOSER_BOND);
    }

    function test_OverlappingParticipantRolesSettleOnce() public {
        MultiProofGame game = _proposeAtAnchor();
        vm.prank(proposer);
        game.challenge();
        bytes memory proof = abi.encodePacked(game.rootId());
        game.submitProofLane(_compact(0, proposer, proof));
        game.submitProofLane(_compact(1, proposer, proof));
        game.resolve();

        _passAirgap(game);
        game.closeGame();

        assertEq(bondVault.availableBalance(proposer), 100 * TOKEN_UNIT);
    }

    function test_Settle_RejectsUnregisteredGame() public {
        UnregisteredGameHarness game = new UnregisteredGameHarness();
        IERC20StakingVault.Payout[] memory payouts = new IERC20StakingVault.Payout[](0);

        vm.prank(address(game));
        vm.expectRevert(abi.encodeWithSelector(GameNotRegistered.selector, address(game)));
        bondVault.settle(payouts);
    }

    function test_Settle_RejectsInvalidPayoutTotal() public {
        MultiProofGame game = _proposeAtAnchor();
        IERC20StakingVault.Payout[] memory payouts = new IERC20StakingVault.Payout[](1);
        payouts[0] = IERC20StakingVault.Payout({recipient: proposer, amount: PROPOSER_BOND - 1});

        vm.prank(address(game));
        vm.expectRevert(abi.encodeWithSelector(InvalidPayoutTotal.selector, PROPOSER_BOND, PROPOSER_BOND - 1));
        bondVault.settle(payouts);
    }

    function test_Settle_RejectsSecondSettlement() public {
        MultiProofGame game = _proposeAtAnchor();
        IERC20StakingVault.Payout[] memory payouts = new IERC20StakingVault.Payout[](1);
        payouts[0] = IERC20StakingVault.Payout({recipient: proposer, amount: PROPOSER_BOND});

        vm.prank(address(game));
        bondVault.settle(payouts);

        vm.prank(address(game));
        vm.expectRevert(abi.encodeWithSelector(GameAlreadySettled.selector, address(game)));
        bondVault.settle(payouts);
    }
}
