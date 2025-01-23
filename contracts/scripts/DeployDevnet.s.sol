// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Script} from "@forge-std/Script.sol";
import {PBHEntryPoint} from "../src/PBHEntryPoint.sol";
import {PBHEntryPointImplV1} from "../src/PBHEntryPointImplV1.sol";
import {PBHSignatureAggregator} from "../src/PBHSignatureAggregator.sol";
import {console} from "forge-std/console.sol";
import {EntryPoint} from "@account-abstraction/contracts/core/EntryPoint.sol";
import {IWorldID} from "@world-id-contracts/interfaces/IWorldID.sol";
import {IPBHEntryPoint} from "../src/interfaces/IPBHEntryPoint.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";

contract DeployDevnet is Script {
    address public entryPoint;
    address public pbhEntryPoint;
    address public pbhEntryPointImpl;
    address public pbhSignatureAggregator;

    uint256 public constant MAX_PBH_GAS_LIMIT = 10000000;
    address constant worldID = address(0xc0ffee)

    function run() public {
        console.log(
            "Deploying: EntryPoint, PBHEntryPoint, PBHEntryPointImplV1, PBHSignatureAggregator"
        );
        uint256 pk = vm.envUint("PRIVATE_KEY");
        vm.startBroadcast(pk);
        deployWorldID();
        deployEntryPoint();
        deployPBHEntryPoint();
        deployPBHSignatureAggregator();
        vm.stopBroadcast();
    }

    function deployEntryPoint() public {
        entryPoint = address(new EntryPoint());
        console.log("EntryPoint Deployed at: ", entryPoint);
    }

    function deployPBHEntryPoint() public {
        pbhEntryPointImpl = address(new PBHEntryPointImplV1());
        console.log("PBHEntryPointImplV1 Deployed at: ", pbhEntryPointImpl);
        bytes memory initCallData = abi.encodeCall(
            PBHEntryPointImplV1.initialize,
            (
                IWorldID(worldIdGroups),
                IEntryPoint(entryPoint),
                255,
                address(0x123),
                MAX_PBH_GAS_LIMIT
            )
        );
        pbhEntryPoint = address(
            new PBHEntryPoint(pbhEntryPointImpl, initCallData)
        );
        console.log("PBHEntryPoint Deployed at: ", pbhEntryPoint);
    }

    function deployPBHSignatureAggregator() public {
        pbhSignatureAggregator = address(
            new PBHSignatureAggregator(pbhEntryPoint, address(worldIDOrb))
        );
        console.log(
            "PBHSignatureAggregator Deployed at: ",
            pbhSignatureAggregator
        );
    }
}
