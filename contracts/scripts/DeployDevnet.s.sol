// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Script} from "@forge-std/Script.sol";
import {PBHEntryPoint} from "../src/PBHEntryPoint.sol";
import {PBHEntryPointImplV1} from "../src/PBHEntryPointImplV1.sol";
import {PBHSignatureAggregator} from "../src/PBHSignatureAggregator.sol";
import {console} from "forge-std/console.sol";
import {EntryPoint} from "@account-abstraction/contracts/core/EntryPoint.sol";
import {WorldIDIdentityManager} from "@world-id-contracts/WorldIDIdentityManager.sol";
import {WorldIDRouter} from "@world-id-contracts/WorldIDRouter.sol";
import {IWorldID} from "@world-id-contracts/interfaces/IWorldID.sol";
import {IPBHEntryPoint} from "../src/interfaces/IPBHEntryPoint.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";
import "@world-id-contracts/WorldIDRouter.sol";
import "@world-id-contracts/WorldIDRouterImplV1.sol";
import "@world-id-contracts/WorldIDIdentityManager.sol";
import "@world-id-contracts/WorldIDIdentityManagerImplV1.sol";
import "@world-id-contracts/WorldIDIdentityManagerImplV2.sol";
import "@world-id-contracts/SemaphoreVerifier.sol";

import {Verifier as InsertionB10} from "@world-id-contracts/verifiers/insertion/b10.sol";
import {Verifier as InsertionB100} from "@world-id-contracts/verifiers/insertion/b100.sol";
import {Verifier as InsertionB600} from "@world-id-contracts/verifiers/insertion/b600.sol";
import {Verifier as InsertionB1200} from "@world-id-contracts/verifiers/insertion/b1200.sol";

import {Verifier as DeletionB10} from "@world-id-contracts/verifiers/deletion/b10.sol";
import {Verifier as DeletionB100} from "@world-id-contracts/verifiers/deletion/b100.sol";

contract DeployDevnet is Script {
    WorldIDIdentityManager worldIDOrb;
    WorldIDIdentityManager worldID;
    address public entryPoint;
    address public worldIdGroups;
    address public pbhEntryPoint;
    address public pbhEntryPointImpl;
    address public pbhSignatureAggregator;

    uint8 constant TREE_DEPTH = 30;
    uint256 constant INITIAL_ROOT = 2331208655746773478043068354535648778756593025616855591272936751328757051821;
    uint256 public constant MAX_PBH_GAS_LIMIT = 10000000;

    address semaphoreVerifier = address(0);

    address batchInsertionVerifiers = address(0);
    address batchDeletionVerifiers = address(0);

    function run() public {
        console.log(
            "Deploying: EntryPoint, PBHEntryPoint, PBHEntryPointImplV1, PBHSignatureAggregator, WorldIDRouter, WorldIDOrb"
        );

        worldIDOrb = deployWorldID(INITIAL_ROOT);
        console.log("WorldIDOrb Deployed at:", address(worldIDOrb));

        WorldIDRouter router = deployWorldIDRouter(IWorldID(address(worldIDOrb)));
        console.log("WorldIDRouter Deployed at: ", address(router));

        // Add WorldIDOrb to the router again for backwards compatibility
        // a lot of services assume it's at group id 1
        updateGroup(address(router), 1, address(worldIDOrb));
        worldIdGroups = address(router);

        beginBroadcast();
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
            (IWorldID(worldIdGroups), IEntryPoint(entryPoint), 255, address(0x123), MAX_PBH_GAS_LIMIT)
        );
        pbhEntryPoint = address(new PBHEntryPoint(pbhEntryPointImpl, initCallData));
        console.log("PBHEntryPoint Deployed at: ", pbhEntryPoint);
    }

    function deployPBHSignatureAggregator() public {
        pbhSignatureAggregator = address(new PBHSignatureAggregator(pbhEntryPoint, address(worldIDOrb)));
        console.log("PBHSignatureAggregator Deployed at: ", pbhSignatureAggregator);
    }

    function deployWorldID(uint256 _initalRoot) public returns (WorldIDIdentityManager worldID) {
        VerifierLookupTable batchInsertionVerifiers = deployInsertionVerifiers();
        VerifierLookupTable batchUpdateVerifiers = deployVerifierLookupTable();
        VerifierLookupTable batchDeletionVerifiers = deployDeletionVerifiers();

        SemaphoreVerifier _semaphoreVerifier = deploySemaphoreVerifier();

        beginBroadcast();
        // Encode:
        // 'initialize(
        //    uint8 _treeDepth,
        //    uint256 initialRoot,
        //    address _batchInsertionVerifiers,
        //    address _batchUpdateVerifiers,
        //    address _semaphoreVerifier
        //  )'
        bytes memory initializeCall = abi.encodeWithSignature(
            "initialize(uint8,uint256,address,address,address)",
            TREE_DEPTH,
            _initalRoot,
            batchInsertionVerifiers,
            batchUpdateVerifiers,
            semaphoreVerifier
        );

        // Encode:
        // 'initializeV2(VerifierLookupTable _batchDeletionVerifiers)'
        bytes memory initializeV2Call = abi.encodeWithSignature("initializeV2(address)", batchDeletionVerifiers);

        WorldIDIdentityManagerImplV1 impl1 = new WorldIDIdentityManagerImplV1();
        WorldIDIdentityManagerImplV2 impl2 = new WorldIDIdentityManagerImplV2();

        worldID = new WorldIDIdentityManager(address(impl1), initializeCall);

        // Recast to access api
        WorldIDIdentityManagerImplV1 worldIDImplV1 = WorldIDIdentityManagerImplV1(address(worldID));
        worldIDImplV1.upgradeToAndCall(address(impl2), initializeV2Call);

        vm.stopBroadcast();

        return worldID;
    }

    function deployWorldIDRouter(IWorldID initialGroupIdentityManager) public returns (WorldIDRouter router) {
        beginBroadcast();

        // Encode:
        // 'initialize(IWorldID initialGroupIdentityManager)'
        bytes memory initializeCall =
            abi.encodeWithSignature("initialize(address)", address(initialGroupIdentityManager));

        WorldIDRouterImplV1 impl = new WorldIDRouterImplV1();
        WorldIDRouter router = new WorldIDRouter(address(impl), initializeCall);

        vm.stopBroadcast();

        return router;
    }

    function deployVerifierLookupTable() public returns (VerifierLookupTable lut) {
        beginBroadcast();

        VerifierLookupTable lut = new VerifierLookupTable();

        vm.stopBroadcast();

        return lut;
    }

    function deploySemaphoreVerifier() public returns (SemaphoreVerifier) {
        if (semaphoreVerifier == address(0)) {
            beginBroadcast();

            SemaphoreVerifier verifier = new SemaphoreVerifier();
            semaphoreVerifier = address(verifier);

            vm.stopBroadcast();
        }

        return SemaphoreVerifier(semaphoreVerifier);
    }

    function deployInsertionVerifiers() public returns (VerifierLookupTable lut) {
        if (batchInsertionVerifiers == address(0)) {
            VerifierLookupTable lut = deployVerifierLookupTable();
            batchInsertionVerifiers = address(lut);

            beginBroadcast();

            lut.addVerifier(10, ITreeVerifier(address(new InsertionB10())));
            lut.addVerifier(100, ITreeVerifier(address(new InsertionB100())));
            lut.addVerifier(600, ITreeVerifier(address(new InsertionB600())));
            lut.addVerifier(1200, ITreeVerifier(address(new InsertionB1200())));

            vm.stopBroadcast();
        }

        return VerifierLookupTable(batchInsertionVerifiers);
    }

    function deployDeletionVerifiers() public returns (VerifierLookupTable lut) {
        if (batchDeletionVerifiers == address(0)) {
            VerifierLookupTable lut = deployVerifierLookupTable();
            batchDeletionVerifiers = address(lut);

            beginBroadcast();

            lut.addVerifier(10, ITreeVerifier(address(new DeletionB10())));
            lut.addVerifier(100, ITreeVerifier(address(new DeletionB100())));

            vm.stopBroadcast();
        }

        return VerifierLookupTable(batchDeletionVerifiers);
    }

    function updateGroup(address router, uint256 groupNumber, address worldID) public {
        WorldIDRouterImplV1 routerImpl = WorldIDRouterImplV1(router);

        beginBroadcast();

        uint256 groupCount = routerImpl.groupCount();
        if (groupCount == groupNumber) {
            routerImpl.addGroup(IWorldID(worldID));
        } else if (groupCount < groupNumber) {
            routerImpl.updateGroup(groupNumber, IWorldID(worldID));
        } else {
            revert("Cannot update group number - group must be added first");
        }

        vm.stopBroadcast();
    }

    function beginBroadcast() internal {
        uint256 deployerPrivateKey = 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80;
        vm.startBroadcast(deployerPrivateKey);
    }
}
