// SPDX-License-Identifier: MIT
pragma solidity ^0.8.21;

import {Setup} from "./Setup.sol";
import {IWorldIDGroups} from "@world-id-contracts/interfaces/IWorldIDGroups.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";
import {PBHEntryPointImplV1} from "../src/PBHEntryPointImplV1.sol";
import {IPBHEntryPoint} from "../src/interfaces/IPBHEntryPoint.sol";
import {PBHEntryPoint} from "../src/PBHEntryPoint.sol";

/// @title PBHVerifier Construction Tests
/// @notice Contains tests for the PBH Verifier construction
/// @author Worldcoin
/// @dev This test suite tests both the proxy and the functionality of the underlying implementation
///      so as to test everything in the context of how it will be deployed.
contract PBHEntryPointConstruction is Setup {
    /// @notice Taken from Initializable.sol
    event Initialized(uint8 version);

    /// @notice Tests if it is possible to construct a router without a delegate.
    function testCanConstructPBHVerifierWithNoDelegate() public {
        // Setup
        address dummy = address(this);
        bytes memory data = new bytes(0x0);

        // Test
        pbhEntryPoint = IPBHEntryPoint(address(new PBHEntryPoint(dummy, data)));
    }

    /// @notice Tests that it is possible to properly construct and initialise a router.
    function testCanConstructRouterWithDelegate(IWorldIDGroups dummy, IEntryPoint entryPoint) public {
        // Setup
        vm.expectEmit(true, true, true, true);
        emit Initialized(1);
        pbhEntryPointImpl = address(new PBHEntryPointImplV1());
        bytes memory callData = abi.encodeCall(IPBHEntryPoint.initialize, (dummy, entryPoint, 30));

        // Test
        pbhEntryPoint = IPBHEntryPoint(address(new PBHEntryPoint(address(pbhEntryPointImpl), callData)));
    }
}
