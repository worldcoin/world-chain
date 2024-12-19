// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Test.sol";
import "@account-abstraction/contracts/interfaces/PackedUserOperation.sol";
import {MockWorldIDGroups} from "./mocks/MockWorldIDGroups.sol";
import {IPBHEntryPoint} from "../src/interfaces/IPBHEntryPoint.sol";
import {PBHSignatureAggregator} from "../src/PBHSignatureAggregator.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";
import {IAggregator} from "@account-abstraction/contracts/interfaces/IAggregator.sol";
import {IWorldIDGroups} from "@world-id-contracts/interfaces/IWorldIDGroups.sol";
import {IAccount} from "@account-abstraction/contracts/interfaces/IAccount.sol";
import {MockAccount} from "./mocks/MockAccount.sol";
import {PBHEntryPointImplV1} from "../src/PBHEntryPointImplV1.sol";
import {PBHEntryPoint} from "../src/PBHEntryPoint.sol";

/// @title Test Setup Contract.
/// @author Worldcoin
/// @dev This test suite tests both the proxy and the functionality of the underlying implementation
///      so as to test everything in the context of how it will be deployed.
contract Setup is Test {
    ///////////////////////////////////////////////////////////////////////////////
    ///                                TEST DATA                                ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice The 4337 Entry Point on Ethereum Mainnet.
    IEntryPoint internal entryPoint = IEntryPoint(address(0x5FF137D4b0FDCD49DcA30c7CF57E578a026d2789));
    /// @notice The PBHEntryPoint contract.
    IPBHEntryPoint public pbhEntryPoint;
    /// @notice The PBHSignatureAggregator contract.
    IAggregator public pbhAggregator;
    /// @notice No-op account.
    IAccount public safe;
    /// @notice The Mock World ID Groups contract.
    IWorldIDGroups public worldIDGroups;

    address public pbhEntryPointImpl;
    address public immutable thisAddress = address(this);
    address public constant nullAddress = address(0);
    ///////////////////////////////////////////////////////////////////////////////
    ///                            TEST ORCHESTRATION                           ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice This function runs before every single test.
    /// @dev It is run before every single iteration of a property-based fuzzing test.
    function setUp() public virtual {
        deployWorldIDGroups();
        deployPBHEntryPoint(worldIDGroups, entryPoint);
        deployPBHSignatureAggregator(address(pbhEntryPoint));
        deploySafeAccount(address(pbhAggregator));

        // Label the addresses for better errors.
        vm.label(address(entryPoint), "ERC-4337 Entry Point");
        vm.label(address(pbhAggregator), "PBH Signature Aggregator");
        vm.label(address(safe), "Safe Account");
        vm.label(address(worldIDGroups), "Mock World ID Groups");
        vm.label(address(pbhEntryPoint), "PBH Entry Point");
        vm.label(pbhEntryPointImpl, "PBH Entry Point Impl V1");

        vm.deal(address(this), type(uint128).max);
        vm.deal(address(safe), type(uint128).max);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              TEST UTILITIES                             ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Initializes a new router.
    /// @dev It is constructed in the globals.
    ///
    /// @param initialGroupAddress The initial group's identity manager.
    /// @param initialEntryPoint The initial entry point.
    function deployPBHEntryPoint(IWorldIDGroups initialGroupAddress, IEntryPoint initialEntryPoint) public {
        pbhEntryPointImpl = address(new PBHEntryPointImplV1());
        bytes memory initCallData =
            abi.encodeCall(PBHEntryPointImplV1.initialize, (initialGroupAddress, initialEntryPoint, 30));
        vm.expectEmit(true, true, true, true);
        emit PBHEntryPointImplV1.PBHEntryPointImplInitialized(initialGroupAddress, initialEntryPoint, 30);
        pbhEntryPoint = IPBHEntryPoint(address(new PBHEntryPoint(pbhEntryPointImpl, initCallData)));
    }

    /// @notice Initializes a new PBHSignatureAggregator.
    /// @dev It is constructed in the globals.
    function deployPBHSignatureAggregator(address _pbhEntryPoint) public {
        pbhAggregator = new PBHSignatureAggregator(_pbhEntryPoint);
    }

    /// @notice Initializes a new safe account.
    /// @dev It is constructed in the globals.
    function deploySafeAccount(address _pbhSignatureAggregator) public {
        safe = new MockAccount(_pbhSignatureAggregator);
    }

    /// @notice Initializes a new World ID Groups contract.
    /// @dev It is constructed in the globals.
    function deployWorldIDGroups() public {
        worldIDGroups = new MockWorldIDGroups();
    }

    /// @notice Constructs a new router without initializing the delegate.
    /// @dev It is constructed in the globals.
    function makeUninitPBHEntryPoint() public {
        pbhEntryPointImpl = address(new PBHEntryPointImplV1());
        pbhEntryPoint = IPBHEntryPoint(address(new PBHEntryPoint(pbhEntryPointImpl, new bytes(0x0))));
    }

    /// @notice Asserts that making the external call using `callData` on `target` succeeds.
    ///
    /// @param target The target at which to make the call.
    /// @param callData The ABI-encoded call to a function.
    function assertCallSucceedsOn(address target, bytes memory callData) public {
        (bool status,) = target.call(callData);
        assert(status);
    }

    /// @notice Asserts that making the external call using `callData` on `target` succeeds.
    ///
    /// @param target The target at which to make the call.
    /// @param callData The ABI-encoded call to a function.
    /// @param expectedReturnData The expected return data from the function.
    function assertCallSucceedsOn(address target, bytes memory callData, bytes memory expectedReturnData) public {
        (bool status, bytes memory returnData) = target.call(callData);
        assert(status);
        assertEq(expectedReturnData, returnData);
    }

    /// @notice Asserts that making the external call using `callData` on `target` fails.
    ///
    /// @param target The target at which to make the call.
    /// @param callData The ABI-encoded call to a function.
    function assertCallFailsOn(address target, bytes memory callData) public {
        (bool status,) = target.call(callData);
        assert(!status);
    }

    /// @notice Asserts that making the external call using `callData` on `target` fails.
    ///
    /// @param target The target at which to make the call.
    /// @param callData The ABI-encoded call to a function.
    /// @param expectedReturnData The expected return data from the function.
    function assertCallFailsOn(address target, bytes memory callData, bytes memory expectedReturnData) public {
        (bool status, bytes memory returnData) = target.call(callData);
        assert(!status);
        assertEq(expectedReturnData, returnData);
    }

    /// @notice Performs the low-level encoding of the `revert(string)` call's return data.
    /// @dev Equivalent to `abi.encodeWithSignature("Error(string)", reason)`.
    ///
    /// @param reason The string reason for the revert.
    ///
    /// @return data The ABI encoding of the revert.
    function encodeStringRevert(string memory reason) public pure returns (bytes memory data) {
        return abi.encodeWithSignature("Error(string)", reason);
    }
}
