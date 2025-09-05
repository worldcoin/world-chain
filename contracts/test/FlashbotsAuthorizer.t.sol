// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {Test} from "forge-std/Test.sol";
import {FlashbotsAuthorizerRegistry} from "../src/FlashbotsAuthorizer.sol";

contract FlashbotsAuthorizerTest is Test {
    FlashbotsAuthorizerRegistry public registry;

    address public owner = address(0x1);
    address public initialAuthorizer = address(0x2);
    address public newAuthorizer = address(0x3);
    address public nonOwner = address(0x4);

    function setUp() public {
        vm.prank(owner);
        registry = new FlashbotsAuthorizerRegistry(initialAuthorizer);
    }

    function test_constructor_setsInitialAuthorizer() public {
        assertEq(registry.getFlashbotsAuthorizer(), initialAuthorizer);
        assertEq(registry.owner(), owner);
    }

    function test_updateFlashbotsAuthorizer_updatesAuthorizerSuccessfully() public {
        vm.prank(owner);
        registry.updateFlashbotsAuthorizer(newAuthorizer);

        assertEq(registry.getFlashbotsAuthorizer(), newAuthorizer);
    }

    function test_updateFlashbotsAuthorizer_revertsOnZeroAddress() public {
        vm.expectRevert(abi.encodeWithSelector(FlashbotsAuthorizerRegistry.ZeroAddress.selector));

        vm.prank(owner);
        registry.updateFlashbotsAuthorizer(address(0));
    }

    function test_multipleUpdates_maintainCorrectState() public {
        address authorizer1 = address(0x5);
        address authorizer2 = address(0x6);

        // First update
        vm.prank(owner);
        registry.updateFlashbotsAuthorizer(authorizer1);
        assertEq(registry.getFlashbotsAuthorizer(), authorizer1);

        // Second update
        vm.prank(owner);
        registry.updateFlashbotsAuthorizer(authorizer2);
        assertEq(registry.getFlashbotsAuthorizer(), authorizer2);
    }
}
