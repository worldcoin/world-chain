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

contract DeployMultipleSafes is Script {
    // Constants for load testing configuration
    uint256 public constant NUM_SAFES = 1;
    uint256 public constant STAKE_AMOUNT = 0.01 ether;

    Safe public singleton;
    SafeProxyFactory public factory;
    SafeModuleSetup public moduleSetup;
    Mock4337Module public module;

    address public constant ENTRY_POINT = 0x0000000071727De22E5E9d8BAf0edAc6f37da032;
    address public constant PBH_SIGNATURE_AGGREGATOR = 0x8af27Ee9AF538C48C7D2a2c8BD6a40eF830e2489;
    uint40 public constant PBH_NONCE_KEY = uint40(bytes5("pbhtx"));

    address[] public deployedSafes;
    address public owner;
    uint256 public ownerPrivateKey;

    function run() public {
        console.log("Deploying: Safe Infrastructure and", NUM_SAFES, "Safe Proxies");

        uint256 privateKey = vm.envUint("PRIVATE_KEY");

        ownerPrivateKey = uint256(keccak256(abi.encodePacked(block.timestamp, block.prevrandao, privateKey)));
        owner = vm.addr(ownerPrivateKey);

        console.log("Deployer address:", vm.addr(privateKey));
        console.log("Safe Owner address:", owner);
        console.log("Safe Owner private key:", vm.toString(bytes32(ownerPrivateKey)));

        vm.startBroadcast(privateKey);
        deployInfrastructure();
        deployMultipleSafes();
        vm.stopBroadcast();

        writeOutputJson();
    }

    function deployInfrastructure() internal {
        // Deploy SafeModuleSetup
        moduleSetup = new SafeModuleSetup();
        console.log("SafeModuleSetup Deployed at: ", address(moduleSetup));

        // Deploy Safe singleton and factory
        singleton = new Safe();
        console.log("Safe Singleton Deployed at: ", address(singleton));

        // Deploy SafeProxyFactory
        factory = new SafeProxyFactory();
        console.log("SafeProxyFactory Deployed at: ", address(factory));

        // Deploy Mock4337Module
        module = new Mock4337Module(ENTRY_POINT, PBH_SIGNATURE_AGGREGATOR, PBH_NONCE_KEY);
        console.log("Mock4337Module Deployed at: ", address(module));
    }

    function deployMultipleSafes() internal {
        console.log("Deploying", NUM_SAFES, "Safe proxies...");

        // Prepare module initialization
        address[] memory modules = new address[](1);
        modules[0] = address(module);

        // Encode the moduleSetup.enableModules call
        bytes memory moduleSetupCall = abi.encodeCall(SafeModuleSetup.enableModules, (modules));

        // Create owners array with single owner
        address[] memory owners = new address[](1);
        owners[0] = owner;

        for (uint256 i = 0; i < NUM_SAFES; i++) {
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

            // Deploy and initialize Safe proxy with unique salt nonce
            SafeProxy proxy = factory.createProxyWithNonce(
                address(singleton),
                initData,
                i // salt nonce
            );

            // Cast proxy to Safe for easier interaction
            Safe safe = Safe(payable(address(proxy)));
            require(safe.isOwner(owner), "Owner not added to Safe");

            // Store deployed safe address
            deployedSafes.push(address(safe));

            console.log("Safe", i + 1, "Deployed at:", address(safe));

            // Deposit stake amount to EntryPoint for this safe
            IEntryPoint(ENTRY_POINT).depositTo{value: STAKE_AMOUNT}(address(safe));
        }

        console.log("Successfully deployed", NUM_SAFES, "Safe proxies");
    }

    function writeOutputJson() internal {
        string memory json = "deployment";

        // Serialize the data
        vm.serializeAddress(json, "module", address(module));
        vm.serializeAddress(json, "owner", owner);
        vm.serializeString(json, "ownerPrivateKey", vm.toString(bytes32(ownerPrivateKey)));
        vm.serializeString(json, "stakeAmount", vm.toString(STAKE_AMOUNT));
        string memory finalJson = vm.serializeAddress(json, "safes", deployedSafes);

        // Write to file with timestamp to avoid overwriting
        string memory timestamp = vm.toString(block.timestamp);
        string memory outputPath = string.concat("./sepolia_load_test_", timestamp, ".json");
        vm.writeJson(finalJson, outputPath);

        console.log("Deployment data written to:", outputPath);
    }
}
