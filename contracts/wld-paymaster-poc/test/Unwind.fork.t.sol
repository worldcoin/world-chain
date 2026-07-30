// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {Test, console2} from "forge-std/Test.sol";
import {Deploy} from "../script/Deploy.s.sol";
import {Unwind} from "../script/Unwind.s.sol";
import {WLDPaymaster} from "../src/WLDPaymaster.sol";
import {IWldEthOracle} from "../src/interfaces/IWldEthOracle.sol";
import {IStakeManager} from "@account-abstraction/interfaces/IStakeManager.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/**
 * @notice Verifies `script/Unwind.s.sol` recovers every asset from a deployed
 *         paymaster, against live World Chain contracts.
 *
 *         Uses a fork test rather than anvil deliberately: ETH accounting here is
 *         exact and deterministic, whereas anvil forking an OP-stack chain reports
 *         balance deltas that don't reconcile with receipt gas (observed a 0.02 ETH
 *         drop against 2.3M wei of actual gas), which makes it useless for
 *         asserting "the recipient got exactly what left the contract".
 *
 *         WORLDCHAIN_RPC_URL=... forge test --match-path 'test/Unwind.fork.t.sol' -vv
 */
contract UnwindForkTest is Test {
    address constant ENTRYPOINT_V07 = 0x0000000071727De22E5E9d8BAf0edAc6f37da032;
    address constant WLD = 0x2cFc85d8E48F8EAB294be644d9E25C3030863003;
    /// @dev `accumulatedWld` storage slot (from `forge inspect storage-layout`).
    uint256 constant SLOT_ACCUMULATED_WLD = 11;

    WLDPaymaster paymaster;
    Unwind unwinder;
    address recipient = makeAddr("recipient");
    bool skipped;

    function setUp() public {
        string memory rpc = vm.envOr("WORLDCHAIN_RPC_URL", string(""));
        if (bytes(rpc).length == 0) {
            skipped = true;
            return;
        }
        vm.createSelectFork(rpc);
        vm.deal(tx.origin, 10 ether);

        (paymaster,) = new Deploy().run();
        unwinder = new Unwind();
        // Unwind's owner-only calls go through vm.startBroadcast(), so they execute
        // as tx.origin — which is exactly who Deploy left as owner. Don't transfer.
        assertEq(paymaster.owner(), tx.origin, "owner is the broadcaster");
    }

    /// @dev Books `booked` WLD for settlement plus `stray` unbooked WLD.
    function _seed(uint256 booked, uint256 stray) internal {
        deal(WLD, address(paymaster), booked + stray);
        vm.store(address(paymaster), bytes32(SLOT_ACCUMULATED_WLD), bytes32(booked));
        assertEq(paymaster.accumulatedWld(), booked, "seeded accumulatedWld");
    }

    // =========================================================================

    /// @dev The headline case: swap booked WLD, sweep stray WLD, take the deposit,
    ///      unlock the stake — and account for every wei and every token.
    function test_Unwind_RecoversEverything() public {
        vm.skip(skipped);

        uint256 booked = 300e18;
        uint256 stray = 50e18;
        _seed(booked, stray);
        vm.roll(block.number + paymaster.blocksPerBatch());
        assertTrue(paymaster.batchReady(), "batch due");

        uint256 depositBefore = paymaster.getDeposit();
        uint256 ethBefore = recipient.balance;
        uint256 wldBefore = IERC20(WLD).balanceOf(recipient);

        unwinder.unwind(paymaster, recipient, false);

        // Booked WLD became ETH and left with the deposit; stray WLD was swept.
        uint256 ethGained = recipient.balance - ethBefore;
        assertGt(ethGained, depositBefore, "recovered deposit plus batch-swap proceeds");
        assertEq(IERC20(WLD).balanceOf(recipient) - wldBefore, stray, "stray WLD swept exactly");

        // Nothing left behind anywhere.
        assertEq(paymaster.getDeposit(), 0, "deposit drained");
        assertEq(paymaster.accumulatedWld(), 0, "nothing still booked");
        assertEq(IERC20(WLD).balanceOf(address(paymaster)), 0, "no WLD left");
        assertEq(address(paymaster).balance, 0, "no ETH stranded");

        // Stake is unlocked but not yet withdrawable.
        IStakeManager.DepositInfo memory info = IStakeManager(ENTRYPOINT_V07).getDepositInfo(address(paymaster));
        assertFalse(info.staked, "stake unlocked");
        assertGt(info.stake, 0, "stake still held pending the delay");
        assertGt(info.withdrawTime, block.timestamp, "withdrawTime in the future");

        console2.log("ETH recovered:", ethGained);
        console2.log("WLD recovered:", stray);
        console2.log("stake pending:", info.stake);
    }

    /// @dev Phase two: the stake comes back once the delay elapses.
    function test_ClaimStake_AfterDelay() public {
        vm.skip(skipped);

        unwinder.unwind(paymaster, recipient, true);

        IStakeManager.DepositInfo memory info = IStakeManager(ENTRYPOINT_V07).getDepositInfo(address(paymaster));
        uint256 staked = info.stake;
        assertGt(staked, 0);

        vm.warp(info.withdrawTime);
        uint256 ethBefore = recipient.balance;
        unwinder.claimStake(paymaster, recipient);

        assertEq(recipient.balance - ethBefore, staked, "full stake returned");
        info = IStakeManager(ENTRYPOINT_V07).getDepositInfo(address(paymaster));
        assertEq(info.stake, 0, "nothing staked anymore");
    }

    function test_RevertWhen_ClaimStakeTooEarly() public {
        vm.skip(skipped);

        unwinder.unwind(paymaster, recipient, true);
        vm.expectRevert(bytes("unstake delay has not elapsed"));
        unwinder.claimStake(paymaster, recipient);
    }

    function test_RevertWhen_ClaimStakeBeforeUnlock() public {
        vm.skip(skipped);

        vm.expectRevert(bytes("stake still locked - run ACTION=unwind first to unlockStake"));
        unwinder.claimStake(paymaster, recipient);
    }

    /// @dev With the swap skipped, booked WLD is deliberately left behind —
    ///      `sweepExcessWld` must not raid the settlement balance.
    function test_Unwind_SkipSwap_LeavesBookedWldUntouched() public {
        vm.skip(skipped);

        uint256 booked = 300e18;
        _seed(booked, 50e18);

        unwinder.unwind(paymaster, recipient, true);

        assertEq(paymaster.accumulatedWld(), booked, "booked WLD untouched");
        assertEq(IERC20(WLD).balanceOf(address(paymaster)), booked, "exactly the booked amount remains");
        assertEq(IERC20(WLD).balanceOf(recipient), 50e18, "only the stray WLD was swept");
        assertEq(paymaster.getDeposit(), 0, "deposit still fully recovered");
    }

    /// @dev Re-running after the batch window opens drains the previously stuck WLD.
    function test_Unwind_SecondPassDrainsStuckWld() public {
        vm.skip(skipped);

        _seed(300e18, 0);
        unwinder.unwind(paymaster, recipient, true); // leaves the booked WLD
        assertEq(paymaster.accumulatedWld(), 300e18);

        vm.roll(block.number + paymaster.blocksPerBatch());
        uint256 ethBefore = recipient.balance;
        unwinder.unwind(paymaster, recipient, false);

        assertEq(paymaster.accumulatedWld(), 0, "stuck WLD drained on the second pass");
        assertEq(IERC20(WLD).balanceOf(address(paymaster)), 0, "no WLD left");
        assertGt(recipient.balance - ethBefore, 0, "swap proceeds recovered");
    }

    /// @dev Unwinding an already-empty paymaster must be a no-op, not a revert.
    function test_Unwind_IsIdempotent() public {
        vm.skip(skipped);

        unwinder.unwind(paymaster, recipient, true);
        uint256 ethAfterFirst = recipient.balance;

        unwinder.unwind(paymaster, recipient, true); // second pass: nothing to do
        assertEq(recipient.balance, ethAfterFirst, "no double payout");
        assertEq(paymaster.getDeposit(), 0);
    }

    function test_RevertWhen_RecipientIsZero() public {
        vm.skip(skipped);

        vm.expectRevert(bytes("RECIPIENT is the zero address"));
        unwinder.unwind(paymaster, address(0), true);
    }
}
