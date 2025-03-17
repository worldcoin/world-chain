// SPDX-License-Identifier: MIT
pragma solidity ^0.8.21;

import {Test} from "forge-std/Test.sol";
import {IWorldIDGroups} from "@world-id-contracts/interfaces/IWorldIDGroups.sol";
import {CheckInitialized} from "@world-id-contracts/utils/CheckInitialized.sol";
import {Ownable2StepUpgradeable} from "@openzeppelin/contracts-upgradeable/access/Ownable2StepUpgradeable.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {WorldIDImpl} from "@world-id-contracts/abstract/WorldIDImpl.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";
import {PBHEntryPoint} from "../src/PBHEntryPoint.sol";
import {PBHEntryPointImplV1} from "../src/PBHEntryPointImplV1.sol";
import {IPBHEntryPoint} from "../src/interfaces/IPBHEntryPoint.sol";
import {IMulticall3} from "../src/interfaces/IMulticall3.sol";
import {IWorldID} from "@world-id-contracts/interfaces/IWorldID.sol";
import {TestSetup} from "./TestSetup.sol";

/// @title PBHEntryPointImplV1InitTest
/// @notice Contains tests asserting the correct initialization of the PBHEntryPointImplV1 contract
/// @author Worldcoin
contract PBHEntryPointImplV1InitTest is Test, TestSetup {
    IPBHEntryPoint uninitializedPBHEntryPoint;

    function setUp() public override {
        address pbhEntryPointImpl = address(new PBHEntryPointImplV1());
        uninitializedPBHEntryPoint = IPBHEntryPoint(address(new PBHEntryPoint(pbhEntryPointImpl, new bytes(0x0))));
    }

    function test_initialize(IWorldID worldId, IEntryPoint entryPoint, uint8 numPbh, address multicall) public {
        vm.assume(address(worldId) != address(0) && address(entryPoint) != address(0) && multicall != address(0));
        vm.assume(numPbh > 0);

        address pbhEntryPointImpl = address(new PBHEntryPointImplV1());
        bytes memory initCallData = abi.encodeCall(
            PBHEntryPointImplV1.initialize,
            (worldId, entryPoint, numPbh, multicall, MAX_PBH_GAS_LIMIT, AUTHORIZED_BUILDERS)
        );

        vm.expectEmit(true, true, true, true);
        emit PBHEntryPointImplV1.PBHEntryPointImplInitialized(
            worldId, entryPoint, numPbh, multicall, MAX_PBH_GAS_LIMIT, AUTHORIZED_BUILDERS
        );
        IPBHEntryPoint(address(new PBHEntryPoint(pbhEntryPointImpl, initCallData)));
    }

    function test_initialize_RevertIf_AddressZero() public {
        IWorldID worldId = IWorldID(address(1));
        IEntryPoint entryPoint = IEntryPoint(address(2));
        uint8 numPbh = 30;
        address multicall = address(3);

        address pbhEntryPointImpl = address(new PBHEntryPointImplV1());

        // Expect revert when entrypoint is address(0)
        bytes memory initCallData = abi.encodeCall(
            PBHEntryPointImplV1.initialize,
            (worldId, IEntryPoint(address(0)), numPbh, multicall, MAX_PBH_GAS_LIMIT, AUTHORIZED_BUILDERS)
        );
        vm.expectRevert(PBHEntryPointImplV1.AddressZero.selector);
        IPBHEntryPoint(address(new PBHEntryPoint(pbhEntryPointImpl, initCallData)));

        // Expect revert when multicall3 is address(0)
        initCallData = abi.encodeCall(
            PBHEntryPointImplV1.initialize,
            (worldId, entryPoint, numPbh, address(0), MAX_PBH_GAS_LIMIT, AUTHORIZED_BUILDERS)
        );
        vm.expectRevert(PBHEntryPointImplV1.AddressZero.selector);
        IPBHEntryPoint(address(new PBHEntryPoint(pbhEntryPointImpl, initCallData)));
    }

    function test_initialize_RevertIf_InvalidNumPbhPerMonth() public {
        IWorldID worldId = IWorldID(address(1));
        IEntryPoint entryPoint = IEntryPoint(address(2));
        uint8 numPbh = 0;
        address multicall = address(3);

        address pbhEntryPointImpl = address(new PBHEntryPointImplV1());

        bytes memory initCallData = abi.encodeCall(
            PBHEntryPointImplV1.initialize,
            (worldId, entryPoint, numPbh, multicall, MAX_PBH_GAS_LIMIT, AUTHORIZED_BUILDERS)
        );
        vm.expectRevert(PBHEntryPointImplV1.InvalidNumPbhPerMonth.selector);
        IPBHEntryPoint(address(new PBHEntryPoint(pbhEntryPointImpl, initCallData)));
    }

    function test_initialize_RevertIf_AlreadyInitialized() public {
        IWorldID worldId = IWorldID(address(1));
        IEntryPoint entryPoint = IEntryPoint(address(2));
        uint8 numPbh = 30;
        address multicall = address(3);

        address pbhEntryPointImpl = address(new PBHEntryPointImplV1());
        bytes memory initCallData = abi.encodeCall(
            PBHEntryPointImplV1.initialize,
            (worldId, entryPoint, numPbh, multicall, MAX_PBH_GAS_LIMIT, AUTHORIZED_BUILDERS)
        );

        vm.expectEmit(true, true, true, true);
        emit PBHEntryPointImplV1.PBHEntryPointImplInitialized(
            worldId, entryPoint, numPbh, multicall, MAX_PBH_GAS_LIMIT, AUTHORIZED_BUILDERS
        );
        IPBHEntryPoint pbhEntryPoint = IPBHEntryPoint(address(new PBHEntryPoint(pbhEntryPointImpl, initCallData)));

        vm.expectRevert("Initializable: contract is already initialized");
        pbhEntryPoint.initialize(worldId, entryPoint, numPbh, multicall, MAX_PBH_GAS_LIMIT, AUTHORIZED_BUILDERS);
    }

    function test_verifyPbh_RevertIf_Uninitialized() public {
        IPBHEntryPoint.PBHPayload memory pbhPayload = IPBHEntryPoint.PBHPayload({
            root: 1,
            pbhExternalNullifier: 0,
            nullifierHash: 1,
            proof: [uint256(0), 0, 0, 0, 0, 0, 0, 0]
        });

        vm.expectRevert(CheckInitialized.ImplementationNotInitialized.selector);
        uninitializedPBHEntryPoint.verifyPbh(0, pbhPayload);
    }

    function test_handleAggregatedOps_RevertIf_Uninitialized() public {
        IEntryPoint.UserOpsPerAggregator[] memory opsPerAggregator;
        address payable beneficiary = payable(address(0));

        vm.expectRevert(CheckInitialized.ImplementationNotInitialized.selector);
        uninitializedPBHEntryPoint.handleAggregatedOps(opsPerAggregator, beneficiary);
    }

    function test_validateSignaturesCallback_RevertIf_Uninitialized() public {
        vm.expectRevert(CheckInitialized.ImplementationNotInitialized.selector);
        uninitializedPBHEntryPoint.validateSignaturesCallback(bytes32(0));
    }

    function test_pbhMulticall_RevertIf_Uninitialized() public {
        IMulticall3.Call3[] memory calls;
        IPBHEntryPoint.PBHPayload memory pbhPayload = IPBHEntryPoint.PBHPayload({
            root: 1,
            pbhExternalNullifier: 0,
            nullifierHash: 1,
            proof: [uint256(0), 0, 0, 0, 0, 0, 0, 0]
        });

        vm.expectRevert(CheckInitialized.ImplementationNotInitialized.selector);
        uninitializedPBHEntryPoint.pbhMulticall(calls, pbhPayload);
    }

    function test_setNumPbhPerMonth_RevertIf_Uninitialized() public {
        vm.expectRevert(CheckInitialized.ImplementationNotInitialized.selector);
        uninitializedPBHEntryPoint.setNumPbhPerMonth(30);
    }

    function test_setWorldId_RevertIf_Uninitialized() public {
        vm.expectRevert(CheckInitialized.ImplementationNotInitialized.selector);
        uninitializedPBHEntryPoint.setWorldId(address(0));
    }
}
