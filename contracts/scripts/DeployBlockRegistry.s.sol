// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.13;

import {Script, console} from "forge-std/Script.sol";
import {WorldChainBlockRegistry} from "../src/WorldChainBlockRegistry.sol";

contract Deploy is Script {
    WorldChainBlockRegistry public worldChainBlockRegistry;
    address public constant BUILDER = 0x7426657A0224e6b1A8aB1863d2D3903dE3325B3d;

    function run() public {
        uint256 deployer = vm.envUint("PRIVATE_KEY");
        vm.startBroadcast(deployer);
        worldChainBlockRegistry = new WorldChainBlockRegistry(BUILDER);
        vm.stopBroadcast();
    }
}