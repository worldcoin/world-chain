// SPDX-License-Identifier: MIT
pragma solidity ^0.8.21;

import {Setup} from "./Setup.sol";
import {IWorldIDGroups} from "@world-id-contracts/interfaces/IWorldIDGroups.sol";

import {CheckInitialized} from "@world-id-contracts/utils/CheckInitialized.sol";
import {Ownable2StepUpgradeable} from "@openzeppelin/contracts-upgradeable/access/Ownable2StepUpgradeable.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {WorldIDImpl} from "@world-id-contracts/abstract/WorldIDImpl.sol";

import {PBHVerifier} from "../src/PBHVerifier.sol";

/// @title World ID PBHEntryPoint Routing Tests
/// @notice Contains tests for the WorldID pbhVerifier
/// @author Worldcoin
/// @dev This test suite tests both the proxy and the functionality of the underlying implementation
///      so as to test everything in the context of how it will be deployed.
contract PBHVerifierRouting is Setup {
    address internal pbhEntryPointAddress;

    function setUp() public override {
        super.setUp();
        pbhEntryPointAddress = address(pbhEntryPoint);
    }

    /// @notice Taken from OwnableUpgradable.sol
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);

    /// @notice Checks that it is possible to get the owner, and that the owner is correctly
    ///         initialised.
    function testHasOwner() public {
        // Setup
        bytes memory callData = abi.encodeCall(OwnableUpgradeable.owner, ());
        bytes memory expectedReturn = abi.encode(address(this));

        // Test
        assertCallSucceedsOn(pbhEntryPointAddress, callData, expectedReturn);
    }

    /// @notice Tests that it is possible to transfer ownership of the contract.
    function testTransferOwner(address newOwner) public {
        // Setup
        vm.assume(newOwner != nullAddress);
        bytes memory transferCallData = abi.encodeCall(Ownable2StepUpgradeable.transferOwnership, (newOwner));
        bytes memory ownerCallData = abi.encodeCall(OwnableUpgradeable.owner, ());
        bytes memory pendingOwnerCallData = abi.encodeCall(Ownable2StepUpgradeable.pendingOwner, ());
        bytes memory acceptOwnerCallData = abi.encodeCall(Ownable2StepUpgradeable.acceptOwnership, ());

        // Test
        assertCallSucceedsOn(pbhEntryPointAddress, transferCallData, new bytes(0x0));
        assertCallSucceedsOn(pbhEntryPointAddress, pendingOwnerCallData, abi.encode(newOwner));
        assertCallSucceedsOn(pbhEntryPointAddress, ownerCallData, abi.encode(thisAddress));

        vm.expectEmit(true, true, true, true);
        emit OwnershipTransferred(thisAddress, newOwner);

        vm.prank(newOwner);
        assertCallSucceedsOn(pbhEntryPointAddress, acceptOwnerCallData, new bytes(0x0));
        assertCallSucceedsOn(pbhEntryPointAddress, ownerCallData, abi.encode(newOwner));
    }

    /// @notice Tests that only the pending owner can accept the ownership transfer.
    function testCannotAcceptOwnershipAsNonPendingOwner(address newOwner, address notNewOwner) public {
        // Setup
        vm.assume(newOwner != nullAddress);
        vm.assume(notNewOwner != newOwner);
        bytes memory callData = abi.encodeCall(Ownable2StepUpgradeable.transferOwnership, (newOwner));
        bytes memory acceptCallData = abi.encodeCall(Ownable2StepUpgradeable.acceptOwnership, ());
        bytes memory expectedError = encodeStringRevert("Ownable2Step: caller is not the new owner");
        assertCallSucceedsOn(pbhEntryPointAddress, callData);
        vm.prank(notNewOwner);

        // Test
        assertCallFailsOn(pbhEntryPointAddress, acceptCallData, expectedError);
    }

    /// @notice Ensures that it is impossible to transfer ownership without being the owner.
    function testCannotTransferOwnerIfNotOwner(address naughty, address newOwner) public {
        // Setup
        vm.assume(naughty != thisAddress && newOwner != nullAddress);
        bytes memory callData = abi.encodeCall(OwnableUpgradeable.transferOwnership, (newOwner));
        bytes memory expectedReturn = encodeStringRevert("Ownable: caller is not the owner");
        vm.prank(naughty);

        // Test
        assertCallFailsOn(pbhEntryPointAddress, callData, expectedReturn);
    }

    /// @notice Tests that it is impossible to renounce ownership, even as the owner.
    function testCannotRenounceOwnershipAsOwner() public {
        // Setup
        bytes memory renounceData = abi.encodeCall(OwnableUpgradeable.renounceOwnership, ());
        bytes memory errorData = abi.encodeWithSelector(WorldIDImpl.CannotRenounceOwnership.selector);

        // Test
        assertCallFailsOn(pbhEntryPointAddress, renounceData, errorData);
    }

    /// @notice Ensures that ownership cannot be renounced by anybody other than the owner.
    function testCannotRenounceOwnershipIfNotOwner(address naughty) public {
        // Setup
        vm.assume(naughty != thisAddress && naughty != nullAddress);
        bytes memory callData = abi.encodeCall(OwnableUpgradeable.renounceOwnership, ());
        bytes memory returnData = encodeStringRevert("Ownable: caller is not the owner");
        vm.prank(naughty);

        // Test
        assertCallFailsOn(pbhEntryPointAddress, callData, returnData);
    }
}
