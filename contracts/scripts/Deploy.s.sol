// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Script} from "@forge-std/Script.sol";
import {PBHEntryPoint} from "../src/PBHEntryPoint.sol";
import {PBHEntryPointImplV1} from "../src/PBHEntryPointImplV1.sol";
import {PBHSignatureAggregator} from "../src/PBHSignatureAggregator.sol";
import {console} from "forge-std/console.sol";
import {IWorldID} from "@world-id-contracts/interfaces/IWorldID.sol";
import {IPBHEntryPoint} from "../src/interfaces/IPBHEntryPoint.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";

contract Deploy is Script {
    address public pbhEntryPoint;
    address public pbhEntryPointImpl;
    address public pbhSignatureAggregator;

    address internal constant WORLD_ID = 0xE177F37AF0A862A02edFEa4F59C02668E9d0aAA4;
    address internal constant ENTRY_POINT = 0x0000000071727De22E5E9d8BAf0edAc6f37da032;
    uint256 internal constant MAX_PBH_GAS_LIMIT = 10500000; // 10.5M 70% of 15M
    uint8 internal constant PBH_NONCE_LIMIT = 30;

    function run() public {
        console.log(
            "Deploying: PBHEntryPoint, PBHEntryPointImplV1, PBHSignatureAggregator"
        );

        uint256 privateKey = vm.envUint("PRIVATE_KEY");
        vm.startBroadcast(privateKey);
        deployPBHEntryPoint();
        deployPBHSignatureAggregator();
        vm.stopBroadcast();
    }

    function deployPBHEntryPoint() public {
        pbhEntryPointImpl = address(new PBHEntryPointImplV1());
        console.log("PBHEntryPointImplV1 Deployed at: ", pbhEntryPointImpl);
        bytes memory initCallData = abi.encodeCall(
            PBHEntryPointImplV1.initialize,
            (
                IWorldID(address(0)),
                IEntryPoint(ENTRY_POINT),
                PBH_NONCE_LIMIT,
                MULTICALL3_ADDRESS,
                MAX_PBH_GAS_LIMIT
            )
        );
        pbhEntryPoint = address(
            new PBHEntryPoint(pbhEntryPointImpl, initCallData)
        );
        console.log("PBHEntryPoint Deployed at: ", pbhEntryPoint);
    }

    function deployPBHSignatureAggregator() public {
        pbhSignatureAggregator = address(new PBHSignatureAggregator(pbhEntryPoint, WORLD_ID));
        console.log("PBHSignatureAggregator Deployed at: ", pbhSignatureAggregator);
    }
}
