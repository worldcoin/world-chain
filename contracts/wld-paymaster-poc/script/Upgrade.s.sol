// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {Script, console2} from "forge-std/Script.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";

import {WLDPaymaster} from "../src/WLDPaymaster.sol";

/**
 * @notice Ships a new WLDPaymaster implementation behind the existing proxy.
 *         Owner-only; run as the paymaster owner.
 *
 * The proxy address never changes, so user WLD approvals, the EntryPoint deposit and
 * stake, and every client's `paymasterAndData` keep working across the upgrade.
 *
 * Pre-flight (all abort before broadcasting):
 *   - PAYMASTER must be a proxy whose implementation slot is non-empty.
 *   - The broadcasting EOA must be the owner, or `upgradeToAndCall` would revert.
 *   - The new implementation must not be the current one (no-op upgrade).
 *
 * Post-flight: the implementation slot points at the new implementation, and the
 * proxy's own state is re-read to confirm it survived — the owner, EntryPoint and
 * WLD address must be unchanged, and pricing must still work.
 *
 * STORAGE COMPATIBILITY IS NOT CHECKED HERE and cannot be, from on-chain data alone.
 * Appending new variables is safe; reordering, removing, or changing the type of an
 * existing one silently corrupts live state. Diff the layouts before running this:
 *
 *   forge inspect src/WLDPaymaster.sol:WLDPaymaster storage-layout
 *
 * against the layout of the deployed version, and bump `version()` when it changes.
 *
 * Usage:
 *   PAYMASTER=0x... forge script script/Upgrade.s.sol:Upgrade \
 *     --rpc-url "$WORLDCHAIN_RPC_URL" --private-key "$PK" --broadcast
 *
 * Env:
 *   PAYMASTER  - the proxy address (required)
 *   INIT_DATA  - optional calldata run on the new implementation via
 *                `upgradeToAndCall`, for a `reinitializer` migration. Default: none.
 */
contract Upgrade is Script {
    function run() external returns (address newImplementation) {
        WLDPaymaster paymaster = WLDPaymaster(payable(vm.envAddress("PAYMASTER")));
        bytes memory initData = vm.envOr("INIT_DATA", bytes(""));

        address current = _implementationOf(address(paymaster));
        require(current != address(0), "PAYMASTER is not an ERC-1967 proxy");
        require(paymaster.owner() == msg.sender, "broadcaster is not the paymaster owner");

        // Snapshot the state the upgrade must not disturb.
        address owner = paymaster.owner();
        address entryPoint = address(paymaster.entryPoint());
        address wld = address(paymaster.wld());
        uint256 accumulatedWld = paymaster.accumulatedWld();
        uint256 deposit = paymaster.getDeposit();

        console2.log("proxy:              ", address(paymaster));
        console2.log("current impl:       ", current);
        console2.log("current version:    ", paymaster.version());

        vm.startBroadcast();

        newImplementation = address(new WLDPaymaster());
        require(newImplementation != current, "new implementation is identical to the current one");
        paymaster.upgradeToAndCall(newImplementation, initData);

        vm.stopBroadcast();

        // --- post-flight ---
        require(_implementationOf(address(paymaster)) == newImplementation, "implementation slot not updated");
        require(paymaster.owner() == owner, "owner changed across the upgrade");
        require(address(paymaster.entryPoint()) == entryPoint, "entryPoint changed across the upgrade");
        require(address(paymaster.wld()) == wld, "wld changed across the upgrade");
        require(paymaster.accumulatedWld() == accumulatedWld, "accumulatedWld changed across the upgrade");
        require(paymaster.getDeposit() == deposit, "deposit changed across the upgrade");
        require(paymaster.quoteWldCharge(0.001 ether) > 0, "pricing broken after the upgrade");

        console2.log("");
        console2.log("=== upgraded ===");
        console2.log("new impl:           ", newImplementation);
        console2.log("new version:        ", paymaster.version());
        console2.log("state preserved: owner, entryPoint, wld, accumulatedWld, deposit");
    }

    function _implementationOf(address proxy) internal view returns (address) {
        return address(uint160(uint256(vm.load(proxy, ERC1967Utils.IMPLEMENTATION_SLOT))));
    }
}
