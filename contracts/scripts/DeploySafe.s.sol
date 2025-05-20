// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Script} from "@forge-std/Script.sol";
import {console} from "forge-std/console.sol";
import {Safe} from "@safe-global/safe-contracts/contracts/Safe.sol";
import {SafeProxyFactory} from "@safe-global/safe-contracts/contracts/proxies/SafeProxyFactory.sol";
import {SafeProxy} from "@safe-global/safe-contracts/contracts/proxies/SafeProxy.sol";
import {Enum} from "@safe-global/safe-contracts/contracts/common/Enum.sol";
import {SafeModuleSetup} from "@4337/SafeModuleSetup.sol";
import {PBHSafe4337Module} from "../src/PBH4337Module.sol";
import {Mock4337Module} from "../test/mocks/Mock4337Module.sol";
import {Safe4337Module} from "@4337/Safe4337Module.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";

contract DeploySafe is Script {
    Safe public singleton;
    SafeProxyFactory public factory;
    SafeModuleSetup public moduleSetup;

    address public constant ENTRY_POINT =
        0x0000000071727De22E5E9d8BAf0edAc6f37da032;
    address public constant PBH_SIGNATURE_AGGREGATOR =
        0x8af27Ee9AF538C48C7D2a2c8BD6a40eF830e2489;
    uint32 public constant PBH_NONCE_KEY = 1123123123;

    function run() public {
        console.log(
            "Deploying: Safe, and PBHSafe4337Module"
        );

        uint256 privateKey = vm.envUint("PRIVATE_KEY");
        uint256 stakeValue = vm.envUint("STAKE_VALUE");
        vm.startBroadcast(privateKey);
        deploySafeAndModule(privateKey, stakeValue);
        vm.stopBroadcast();
    }

    function deploySafeAndModule(uint256 privateKey, uint256 stakeValue) public {
        // Deploy SafeModuleSetup
        moduleSetup = new SafeModuleSetup();
        console.log("SafeModuleSetup Deployed at: ", address(moduleSetup));

        // Deploy Safe singleton and factory
        singleton = new Safe();
        console.log("Safe Singleton Deployed at: ", address(singleton));

        // Deploy SafeProxyFactory
        factory = new SafeProxyFactory();
        console.log("SafeProxyFactory Deployed at: ", address(factory));

        address owner = vm.addr(privateKey);
        console.log("Safe Owner", owner);
        Mock4337Module module = new Mock4337Module(
            ENTRY_POINT,
            PBH_SIGNATURE_AGGREGATOR,
            PBH_NONCE_KEY
        );

        console.log("PBH4337Module Deployed at: ", address(module));
        // Prepare module initialization
        address[] memory modules = new address[](1);
        modules[0] = address(module);

        // Encode the moduleSetup.enableModules call
        bytes memory moduleSetupCall = abi.encodeCall(
            SafeModuleSetup.enableModules,
            (modules)
        );

        // Create owners array with single owner
        address[] memory owners = new address[](1);
        owners[0] = owner;

        // Encode initialization data for proxy
        bytes memory initData = abi.encodeCall(
            Safe.setup,
            (
                owners,
                1, // threshold
                address(moduleSetup), // to
                moduleSetupCall, // data
                address(module), // fallbackHandler
                address(0), // paymentToken
                0, // payment
                payable(address(0)) // paymentReceiver
            )
        );

        // Deploy and initialize Safe proxy
        SafeProxy proxy = factory.createProxyWithNonce(
            address(singleton),
            initData,
            0 // salt nonce
        );

        // Cast proxy to Safe for easier interaction
        Safe safe = Safe(payable(address(proxy)));
        require(safe.isOwner(owner), "Owner not added to Safe");
        console.log("Safe Proxy Deployed at: ", address(safe));

        IEntryPoint(ENTRY_POINT).addStake{value: stakeValue}(86400);
    }
}
