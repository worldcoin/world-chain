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
import {Create2Factory} from "./Create2Deploy.sol";

contract Deploy is Create2Factory, Script {
    address public pbhEntryPoint;
    address public pbhEntryPointImpl;
    address public pbhSignatureAggregator;

    address internal constant WORLD_ID =
        0xE177F37AF0A862A02edFEa4F59C02668E9d0aAA4;
    address internal constant ENTRY_POINT =
        0x0000000071727De22E5E9d8BAf0edAc6f37da032;
    uint256 internal constant MAX_PBH_GAS_LIMIT = 15000000; // 15M
    uint16 internal constant PBH_NONCE_LIMIT = type(uint16).max;
    address[] internal authorizedBuilders = [
        0x0459B1592C4e1A2cFB2F0606fDe0F7D9E7995E9A
    ]; 
    address internal constant OWNER = 0x96d55BD9c8C4706FED243c1e15825FF7854920fA;

    function run() public {
        console.log(
            "Deploying: PBHEntryPoint, PBHEntryPointImplV1, PBHSignatureAggregator"
        );
        bytes32 implSalt = vm.envBytes32("IMPL_SALT");
        bytes32 proxySalt = vm.envBytes32("PROXY_SALT");
        bytes32 signatureAggregatorSalt = vm.envBytes32("AGGREGATOR_SALT");
        uint256 privateKey = vm.envUint("PRIVATE_KEY");

        vm.startBroadcast(privateKey);
        deployPBHEntryPoint(proxySalt, implSalt);
        deployPBHSignatureAggregator(signatureAggregatorSalt);
        vm.stopBroadcast();
    }

    function deployPBHEntryPoint(bytes32 proxySalt, bytes32 implSalt) public {
        pbhEntryPointImpl = deploy(
            implSalt,
            type(PBHEntryPointImplV1).creationCode
        );
        console.log("PBHEntryPointImplV1 Deployed at: ", pbhEntryPointImpl);

        /// @dev Do not modify this for deterministic deployments
        ///     things can be toggled after deployment if needed.
        bytes memory initCallData = abi.encodeCall(
            PBHEntryPointImplV1.initialize,
            (
                IWorldID(address(0)),
                IEntryPoint(ENTRY_POINT),
                PBH_NONCE_LIMIT,
                MAX_PBH_GAS_LIMIT,
                authorizedBuilders,
                OWNER
            )
        );

        bytes memory initCode = abi.encodePacked(
            type(PBHEntryPoint).creationCode,
            abi.encode(pbhEntryPointImpl, initCallData)
        );

        // Note: Theoretically this tx could be front-run which would result in a revert on 
        //       the deployment of the proxy. This is low-risk, and minimal impact as we can just redeploy.
        pbhEntryPoint = deploy(proxySalt, initCode); 
        console.log("PBHEntryPoint Deployed at: ", pbhEntryPoint);
    }

    function deployPBHSignatureAggregator(bytes32 salt) public {
        bytes memory initCode = abi.encodePacked(
            type(PBHSignatureAggregator).creationCode,
            abi.encode(pbhEntryPoint, WORLD_ID)
        );
        pbhSignatureAggregator = deploy(salt, initCode);
        console.log(
            "PBHSignatureAggregator Deployed at: ",
            pbhSignatureAggregator
        );
    }
}
