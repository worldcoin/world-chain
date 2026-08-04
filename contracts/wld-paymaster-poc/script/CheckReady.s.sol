// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {Script, console2} from "forge-std/Script.sol";
import {WLDPaymaster} from "../src/WLDPaymaster.sol";
import {IStakeManager} from "@account-abstraction/interfaces/IStakeManager.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";

/**
 * @notice Read-only readiness check for an already-deployed paymaster. Answers
 *         "can this thing sponsor a UserOp right now?" and, if not, which
 *         precondition is missing. Sends no transactions.
 *
 *   PAYMASTER=0x... forge script script/CheckReady.s.sol:CheckReady \
 *     --rpc-url "$WORLDCHAIN_RPC_URL"
 *
 * Optional:
 *   USER      - also check this account's WLD balance and allowance
 *   MAX_COST  - the op cost to test against, wei (default 0.001e18)
 */
contract CheckReady is Script {
    uint32 constant MIN_UNSTAKE_DELAY = 1 days;

    function run() external view {
        WLDPaymaster paymaster = WLDPaymaster(payable(vm.envAddress("PAYMASTER")));
        uint256 maxCost = vm.envOr("MAX_COST", uint256(0.001 ether));
        address user = vm.envOr("USER", address(0));

        bool ready = true;

        console2.log("paymaster:", address(paymaster));
        console2.log("chain id: ", block.chainid);

        // PAYMASTER is expected to be the proxy. Pointing this at an implementation
        // by mistake reads as "not ready" for confusing reasons, so name it here.
        address impl = address(uint160(uint256(vm.load(address(paymaster), ERC1967Utils.IMPLEMENTATION_SLOT))));
        if (impl == address(0)) {
            console2.log("[warn] no ERC-1967 implementation slot: not a proxy.");
            console2.log("       Is PAYMASTER the implementation instead of the proxy?");
        } else {
            console2.log("implementation:", impl);
            console2.log("version:       ", paymaster.version());
        }
        console2.log("");

        // --- 1. oracle produces a price ---
        uint256 quote;
        try paymaster.quoteWldCharge(maxCost) returns (uint256 q) {
            quote = q;
            console2.log("[ok]   oracle live; WLD charge for maxCost:", q);
        } catch {
            ready = false;
            console2.log("[FAIL] oracle reverted - stale/invalid feed. Ops will be rejected.");
        }

        // --- 2. deposit clears the floor for this op ---
        uint256 deposit = paymaster.getDeposit();
        uint256 floor = paymaster.minEntryPointDeposit();
        console2.log("       deposit:", deposit, " floor:", floor);
        if (deposit < maxCost + floor) {
            ready = false;
            console2.log("[FAIL] deposit < maxCost + floor. Needed:", maxCost + floor);
        } else {
            console2.log("[ok]   deposit covers maxCost + floor; ops sponsorable:", (deposit - floor) / maxCost);
        }

        // --- 3. staked, with a compliant unstake delay ---
        IStakeManager.DepositInfo memory info =
            IStakeManager(address(paymaster.entryPoint())).getDepositInfo(address(paymaster));
        if (!info.staked || info.stake == 0) {
            ready = false;
            console2.log("[FAIL] not staked - bundlers reject validation-time token writes.");
        } else if (info.unstakeDelaySec < MIN_UNSTAKE_DELAY) {
            ready = false;
            console2.log("[FAIL] unstake delay below the 1-day minimum:", info.unstakeDelaySec);
        } else {
            console2.log("[ok]   staked:", info.stake, " unstake delay:", info.unstakeDelaySec);
        }

        // --- 4. batch settlement is configured sanely ---
        if (paymaster.maxWldPerBatch() == 0) {
            console2.log("[warn] maxWldPerBatch=0: an oversized batch can stall settlement.");
        } else {
            console2.log("[ok]   max WLD per batch:", paymaster.maxWldPerBatch());
        }
        console2.log("       accumulated WLD:", paymaster.accumulatedWld());
        console2.log("       batch ready:", paymaster.batchReady());

        // --- 5. optional: is this user able to pay? ---
        if (user != address(0)) {
            IERC20 wld = paymaster.wld();
            uint256 bal = wld.balanceOf(user);
            uint256 allowed = wld.allowance(user, address(paymaster));
            console2.log("");
            console2.log("user:     ", user);
            console2.log("  WLD balance:  ", bal);
            console2.log("  WLD allowance:", allowed);
            if (bal < quote) console2.log("[FAIL] user WLD balance below the charge:", quote);
            if (allowed < quote) console2.log("[FAIL] user must approve() the paymaster for at least", quote);
        }

        console2.log("");
        if (ready) {
            console2.log("=> READY to sponsor.");
        } else {
            console2.log("=> NOT READY - fix the [FAIL] items above.");
        }

        // Owner risk is worth restating on every check.
        console2.log("   owner (can drain deposit / swap oracle):", paymaster.owner());
    }
}
