// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.13;

import {Script, console} from "forge-std/Script.sol";
import {WorldChainBlockRegistry} from "../src/WorldChainBlockRegistry.sol";

contract Deploy is Script {
    WorldChainBlockRegistry public worldChainBlockRegistry;
    address public constant BUILDER = address(0);

    function run() public {
        uint256 deployer = vm.envUint("PRIVATE_KEY");
        vm.startBroadcast(deployer);
        worldChainBlockRegistry = new WorldChainBlockRegistry(BUILDER);
        vm.stopBroadcast();
    }
}