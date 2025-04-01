// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Script} from "@forge-std/Script.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {PBHEntryPointImplV1} from "../src/PBHEntryPointImplV1.sol";
import {PBHSignatureAggregator} from "../src/PBHSignatureAggregator.sol";
import {console} from "forge-std/console.sol";
import {EntryPoint} from "@account-abstraction/contracts/core/EntryPoint.sol";
import {IWorldID} from "@world-id-contracts/interfaces/IWorldID.sol";
import {IPBHEntryPoint} from "../src/interfaces/IPBHEntryPoint.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";
import {Safe} from "@safe-global/safe-contracts/contracts/Safe.sol";
import {SafeProxyFactory} from "@safe-global/safe-contracts/contracts/proxies/SafeProxyFactory.sol";
import {SafeProxy} from "@safe-global/safe-contracts/contracts/proxies/SafeProxy.sol";
import {Enum} from "@safe-global/safe-contracts/contracts/common/Enum.sol";
import {SafeModuleSetup} from "@4337/SafeModuleSetup.sol";
import {PBHSafe4337Module} from "../src/PBH4337Module.sol";
import {Mock4337Module} from "../test/mocks/Mock4337Module.sol";
import {Safe4337Module} from "@4337/Safe4337Module.sol";

contract DeployDevnet is Script {
    address public pbhEntryPoint;
    address public pbhEntryPointImpl;
    address public pbhSignatureAggregator;

    Safe public singleton;
    SafeProxyFactory public factory;
    SafeModuleSetup public moduleSetup;

    address public constant ENTRY_POINT =
        0x0000000071727De22E5E9d8BAf0edAc6f37da032;
    address public constant WORLD_ID =
        0x5FbDB2315678afecb367f032d93F642f64180aa3;
    address public constant BUILDER =
        0x70997970C51812dc3A010C7d01b50e0d17dc79C8;
    /// @dev The root of the Test tree.
    uint256 constant INITIAL_ROOT =
        0x5276AD6D825269EB0B67A2E1589123DED27C8B8EABFA898FF7E878AD61071AD;
    uint256 public constant MAX_PBH_GAS_LIMIT = 30000000;
    uint32 public constant PBH_NONCE_KEY = 1123123123;
    address[] public authorizedBuilders = [BUILDER];

    function run() public {
        console.log(
            "Deploying: EntryPoint, ERC1967Proxy, PBHEntryPointImplV1, PBHSignatureAggregator, PBHSafe4337Module"
        );

        uint256 privateKey = vm.envUint("PRIVATE_KEY");
        vm.startBroadcast(privateKey);
        deployPBHEntryPoint();
        deployPBHSignatureAggregator();
        deploySafeAndModules();
        updateWorldID();
        vm.stopBroadcast();
    }

    function deployPBHEntryPoint() public {
        pbhEntryPointImpl = address(new PBHEntryPointImplV1());
        console.log("PBHEntryPointImplV1 Deployed at: ", pbhEntryPointImpl);
        bytes memory initCallData = abi.encodeCall(
            PBHEntryPointImplV1.initialize,
            (
                IWorldID(WORLD_ID),
                IEntryPoint(ENTRY_POINT),
                255,
                MAX_PBH_GAS_LIMIT,
                authorizedBuilders,
                msg.sender
            )
        );
        pbhEntryPoint = address(
            new ERC1967Proxy(pbhEntryPointImpl, initCallData)
        );
        console.log("ERC1967Proxy Deployed at: ", pbhEntryPoint);
    }

    function deployPBHSignatureAggregator() public {
        pbhSignatureAggregator = address(
            new PBHSignatureAggregator(pbhEntryPoint, WORLD_ID)
        );
        console.log(
            "PBHSignatureAggregator Deployed at: ",
            pbhSignatureAggregator
        );
    }

    function deploySafeAndModules() public {
        uint256 ownerKey0 = vm.envUint("SAFE_OWNER_0");
        uint256 ownerKey1 = vm.envUint("SAFE_OWNER_1");
        uint256 ownerKey2 = vm.envUint("SAFE_OWNER_2");
        uint256 ownerKey3 = vm.envUint("SAFE_OWNER_3");
        uint256 ownerKey4 = vm.envUint("SAFE_OWNER_4");
        uint256 ownerKey5 = vm.envUint("SAFE_OWNER_5");

        uint256[6] memory signers = [
            ownerKey0,
            ownerKey1,
            ownerKey2,
            ownerKey3,
            ownerKey4,
            ownerKey5
        ];

        // Deploy SafeModuleSetup
        moduleSetup = new SafeModuleSetup();
        console.log("SafeModuleSetup Deployed at: ", address(moduleSetup));

        // Deploy Safe singleton and factory
        singleton = new Safe();
        console.log("Safe Singleton Deployed at: ", address(singleton));

        // Deploy SafeProxyFactory
        factory = new SafeProxyFactory();
        console.log("SafeProxyFactory Deployed at: ", address(factory));

        for (uint256 i = 0; i < signers.length; i++) {
            uint256 ownerKey = signers[i];
            address owner = vm.addr(ownerKey);
            console.log("Owner Key", ownerKey);
            console.log("Owner", owner);
            Mock4337Module module = new Mock4337Module(
                ENTRY_POINT,
                pbhSignatureAggregator,
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
            IEntryPoint(ENTRY_POINT).depositTo{value: 1 ether}(address(safe));
        }
    }

    function updateWorldID() public {
        bytes memory data = abi.encodeWithSelector(
            bytes4(keccak256("receiveRoot(uint256)")),
            INITIAL_ROOT
        );

        (bool success, ) = WORLD_ID.call(data);
        require(success, "Failed to update WorldID root");
    }
}
