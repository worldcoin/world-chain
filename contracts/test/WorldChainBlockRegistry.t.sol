// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import "forge-std/Test.sol";
import "src/WorldChainBlockRegistry.sol";

contract WorldChainBlockRegistryTest is Test {
    WorldChainBlockRegistry blockRegistry;
    address builder = address(0xC0FFEE);
    address owner = address(0xB0B);

    function setUp() public {
        vm.prank(owner);
        blockRegistry = new WorldChainBlockRegistry(builder);
    }

    function test_stampBlock() public {
        vm.prank(builder);
        vm.expectEmit(true, true, true, true);
        emit WorldChainBlockRegistry.BuiltBlock(builder, block.number);
        blockRegistry.stampBlock();
        assertEq(blockRegistry.builtBlocks(block.number), builder);
    }

    function test_stampBlock_RevertIf_Unauthorized() public {
        vm.expectRevert(WorldChainBlockRegistry.Unauthorized.selector);
        blockRegistry.stampBlock();
    }

    function test_stampBlock_RevertIf_AlreadyRegistered() public {
        vm.prank(builder);
        blockRegistry.stampBlock();

        vm.prank(builder);
        vm.expectRevert(WorldChainBlockRegistry.BlockAlreadyRegistered.selector);
        blockRegistry.stampBlock();
    }

    function test_updateBuilder() public {
        address newBuilder = address(0xCAFE);
        vm.prank(owner);
        vm.expectEmit(true, true, true, true);
        emit WorldChainBlockRegistry.WorldChainBuilderUpdated(newBuilder);
        blockRegistry.updateBuilder(newBuilder);

        vm.prank(newBuilder);
        blockRegistry.stampBlock();
        assertEq(blockRegistry.builtBlocks(block.number), newBuilder);
    }

    function test_updateBuilder_RevertIf_Unauthorized() public {
        address newBuilder = address(0xCAFE);
        vm.prank(address(0xDEAD));
        vm.expectRevert(WorldChainBlockRegistry.Unauthorized.selector);
        blockRegistry.updateBuilder(newBuilder);
    }
}
