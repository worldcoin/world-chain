// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {Script, console2} from "forge-std/Script.sol";
import {WLDPaymaster} from "../src/WLDPaymaster.sol";
import {IStakeManager} from "@account-abstraction/interfaces/IStakeManager.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/**
 * @notice Recovers everything from a deployed paymaster so the funds can be
 *         reused on a new deployment. Owner-only; run as the paymaster owner.
 *
 * The stake cannot be withdrawn immediately — the EntryPoint enforces the unstake
 * delay — so recovery is two phases:
 *
 *   ACTION=unwind (default)
 *     1. `triggerBatchSwap` if it is due and there is WLD booked, converting
 *        accumulated WLD into EntryPoint deposit. Best-effort: a revert (too
 *        early, slippage) is logged and skipped, not fatal.
 *     2. `sweepExcessWld` — any WLD not booked into `accumulatedWld`.
 *     3. `withdrawTo` the entire EntryPoint deposit.
 *     4. `unlockStake` — starts the unstake-delay clock.
 *
 *     After this the paymaster is drained of ETH and can no longer sponsor.
 *
 *   ACTION=claim-stake
 *     `withdrawStake` once the delay has elapsed. Reverts (with the remaining
 *     wait logged) if called too early.
 *
 * Usage:
 *   PAYMASTER=0x... forge script script/Unwind.s.sol:Unwind \
 *     --rpc-url "$WORLDCHAIN_RPC_URL" --private-key "$PK" --broadcast
 *
 *   # ~1 day later
 *   PAYMASTER=0x... ACTION=claim-stake forge script script/Unwind.s.sol:Unwind \
 *     --rpc-url "$WORLDCHAIN_RPC_URL" --private-key "$PK" --broadcast
 *
 * Env:
 *   PAYMASTER   - the deployed paymaster PROXY (required). The proxy holds the
 *                 deposit, stake and WLD; the implementation holds nothing.
 *   ACTION      - "unwind" (default) or "claim-stake"
 *   RECIPIENT   - where funds go (default: the broadcasting EOA)
 *   SKIP_SWAP   - "true" to skip the batch swap in step 1
 *
 * NOTE on stuck WLD: `sweepExcessWld` deliberately cannot touch `accumulatedWld`,
 * which belongs to the settlement flow. If a batch swap can't execute (thin
 * liquidity, oracle down) that WLD stays in the contract. `triggerBatchSwap` is
 * the only path out, and it is permissionless — anyone can retry it later.
 */
contract Unwind is Script {
    function run() external {
        WLDPaymaster paymaster = WLDPaymaster(payable(vm.envAddress("PAYMASTER")));
        string memory action = vm.envOr("ACTION", string("unwind"));

        address recipient = vm.envOr("RECIPIENT", msg.sender);
        require(recipient != address(0), "RECIPIENT is the zero address");

        if (keccak256(bytes(action)) == keccak256("claim-stake")) {
            _claimStake(paymaster, recipient);
        } else if (keccak256(bytes(action)) == keccak256("unwind")) {
            _unwind(paymaster, recipient, vm.envOr("SKIP_SWAP", false));
        } else {
            revert("ACTION must be 'unwind' or 'claim-stake'");
        }
    }

    // =========================================================================

    /// @notice Unwind with explicit arguments.
    /// @dev Public so tests can drive it without process-global env vars.
    function unwind(WLDPaymaster paymaster, address recipient, bool skipSwap) public {
        require(recipient != address(0), "RECIPIENT is the zero address");
        _unwind(paymaster, recipient, skipSwap);
    }

    /// @notice Claim the stake with an explicit recipient.
    function claimStake(WLDPaymaster paymaster, address recipient) public {
        require(recipient != address(0), "RECIPIENT is the zero address");
        _claimStake(paymaster, recipient);
    }

    function _unwind(WLDPaymaster paymaster, address recipient, bool skipSwap) internal {
        IERC20 wld = paymaster.wld();
        uint256 depositBefore = paymaster.getDeposit();

        console2.log("paymaster:      ", address(paymaster));
        console2.log("recipient:      ", recipient);
        console2.log("deposit:        ", depositBefore);
        console2.log("accumulated WLD:", paymaster.accumulatedWld());
        console2.log("WLD balance:    ", wld.balanceOf(address(paymaster)));
        console2.log("");

        vm.startBroadcast();

        // --- 1. convert booked WLD into deposit, if possible ---
        if (!skipSwap && paymaster.batchReady()) {
            try paymaster.triggerBatchSwap(0) returns (uint256 ethOut) {
                console2.log("[1] batch swap ->", ethOut, "wei added to deposit");
            } catch {
                console2.log("[1] batch swap reverted (too early / slippage); skipped");
            }
        } else {
            console2.log("[1] batch swap not due or skipped");
        }

        // --- 2. sweep unbooked WLD ---
        uint256 balance = wld.balanceOf(address(paymaster));
        uint256 booked = paymaster.accumulatedWld();
        uint256 sweepable = balance > booked ? balance - booked : 0;
        if (sweepable > 0) {
            paymaster.sweepExcessWld(recipient);
            console2.log("[2] swept WLD:", sweepable);
        } else {
            console2.log("[2] no unbooked WLD to sweep");
        }

        // --- 3. withdraw the whole deposit ---
        uint256 deposit = paymaster.getDeposit();
        if (deposit > 0) {
            paymaster.withdrawTo(payable(recipient), deposit);
            console2.log("[3] withdrew deposit:", deposit);
        } else {
            console2.log("[3] deposit already empty");
        }

        // --- 4. start the unstake clock ---
        IStakeManager entryPoint = IStakeManager(address(paymaster.entryPoint()));
        IStakeManager.DepositInfo memory info = entryPoint.getDepositInfo(address(paymaster));
        if (info.staked) {
            paymaster.unlockStake();
            console2.log("[4] stake unlocked:", info.stake);
            console2.log("    withdrawable in (s):", info.unstakeDelaySec);
        } else if (info.stake > 0) {
            console2.log("[4] stake already unlocked; withdrawTime:", info.withdrawTime);
        } else {
            console2.log("[4] nothing staked");
        }

        vm.stopBroadcast();

        uint256 stuck = paymaster.accumulatedWld();
        console2.log("");
        console2.log("=== unwound ===");
        console2.log("deposit remaining: ", paymaster.getDeposit());
        if (stuck > 0) {
            console2.log("WARNING: WLD still booked for settlement:", stuck);
            console2.log("  sweepExcessWld cannot take it. Run triggerBatchSwap once the");
            console2.log("  batch window opens, then re-run this script.");
        }
        console2.log("Next: ACTION=claim-stake after the unstake delay elapses.");
    }

    function _claimStake(WLDPaymaster paymaster, address recipient) internal {
        IStakeManager entryPoint = IStakeManager(address(paymaster.entryPoint()));
        IStakeManager.DepositInfo memory info = entryPoint.getDepositInfo(address(paymaster));

        require(info.stake > 0, "nothing staked");
        require(!info.staked, "stake still locked - run ACTION=unwind first to unlockStake");
        if (block.timestamp < info.withdrawTime) {
            console2.log("withdrawable at:", info.withdrawTime);
            console2.log("now:            ", block.timestamp);
            console2.log("wait (s):       ", info.withdrawTime - block.timestamp);
            revert("unstake delay has not elapsed");
        }

        vm.startBroadcast();
        paymaster.withdrawStake(payable(recipient));
        vm.stopBroadcast();

        console2.log("withdrew stake:", info.stake, "to", recipient);
    }
}
