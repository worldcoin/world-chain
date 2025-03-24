// SPDX-License-Identifier: MIT
pragma solidity ^0.8.21;

import {Test} from "forge-std/Test.sol";
import {IWorldIDGroups} from "@world-id-contracts/interfaces/IWorldIDGroups.sol";
import {CheckInitialized} from "@world-id-contracts/utils/CheckInitialized.sol";
import {Ownable2StepUpgradeable} from "@openzeppelin/contracts-upgradeable/access/Ownable2StepUpgradeable.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
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

    function test_initialize(IWorldID worldId, IEntryPoint entryPoint, uint8 numPbh) public {
        vm.assume(address(worldId) != address(0) && address(entryPoint) != address(0));
        vm.assume(numPbh > 0);

        address pbhEntryPointImpl = address(new PBHEntryPointImplV1());
        bytes memory initCallData = abi.encodeCall(
            PBHEntryPointImplV1.initialize, (worldId, entryPoint, numPbh, MAX_PBH_GAS_LIMIT, AUTHORIZED_BUILDERS, OWNER)
        );

        vm.expectEmit(true, true, true, true);
        emit PBHEntryPointImplV1.PBHEntryPointImplInitialized(
            worldId, entryPoint, numPbh, MAX_PBH_GAS_LIMIT, AUTHORIZED_BUILDERS, OWNER
        );
        IPBHEntryPoint(address(new PBHEntryPoint(pbhEntryPointImpl, initCallData)));
    }

    function test_initialize_RevertIf_AddressZero() public {
        IWorldID worldId = IWorldID(address(1));
        IEntryPoint entryPoint = IEntryPoint(address(0));
        uint8 numPbh = 30;
        address pbhEntryPointImpl = address(new PBHEntryPointImplV1());

        // Expect revert when entrypoint is address(0)
        bytes memory initCallData = abi.encodeCall(
            PBHEntryPointImplV1.initialize,
            (worldId, IEntryPoint(address(0)), numPbh, MAX_PBH_GAS_LIMIT, AUTHORIZED_BUILDERS, OWNER)
        );
        vm.expectRevert(PBHEntryPointImplV1.AddressZero.selector);
        IPBHEntryPoint(address(new PBHEntryPoint(pbhEntryPointImpl, initCallData)));

        // Expect revert when multicall3 is address(0)
        initCallData = abi.encodeCall(
            PBHEntryPointImplV1.initialize, (worldId, entryPoint, numPbh, MAX_PBH_GAS_LIMIT, AUTHORIZED_BUILDERS, OWNER)
        );
        vm.expectRevert(PBHEntryPointImplV1.AddressZero.selector);
        IPBHEntryPoint(address(new PBHEntryPoint(pbhEntryPointImpl, initCallData)));
    }

    function test_initialize_RevertIf_InvalidNumPbhPerMonth() public {
        IWorldID worldId = IWorldID(address(1));
        IEntryPoint entryPoint = IEntryPoint(address(2));
        uint8 numPbh = 0;

        address pbhEntryPointImpl = address(new PBHEntryPointImplV1());

        bytes memory initCallData = abi.encodeCall(
            PBHEntryPointImplV1.initialize, (worldId, entryPoint, numPbh, MAX_PBH_GAS_LIMIT, AUTHORIZED_BUILDERS, OWNER)
        );
        vm.expectRevert(PBHEntryPointImplV1.InvalidNumPbhPerMonth.selector);
        IPBHEntryPoint(address(new PBHEntryPoint(pbhEntryPointImpl, initCallData)));
    }

    function test_initialize_RevertIf_AlreadyInitialized() public {
        IWorldID worldId = IWorldID(address(1));
        IEntryPoint entryPoint = IEntryPoint(address(2));
        uint8 numPbh = 30;

        address pbhEntryPointImpl = address(new PBHEntryPointImplV1());
        bytes memory initCallData = abi.encodeCall(
            PBHEntryPointImplV1.initialize, (worldId, entryPoint, numPbh, MAX_PBH_GAS_LIMIT, AUTHORIZED_BUILDERS, OWNER)
        );

        vm.expectEmit(true, true, true, true);
        emit PBHEntryPointImplV1.PBHEntryPointImplInitialized(
            worldId, entryPoint, numPbh, MAX_PBH_GAS_LIMIT, AUTHORIZED_BUILDERS, OWNER
        );
        IPBHEntryPoint pbhEntryPoint = IPBHEntryPoint(address(new PBHEntryPoint(pbhEntryPointImpl, initCallData)));

        vm.expectRevert(Initializable.InvalidInitialization.selector);
        pbhEntryPoint.initialize(worldId, entryPoint, numPbh, MAX_PBH_GAS_LIMIT, AUTHORIZED_BUILDERS, OWNER);
    }
}
