// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import "@openzeppelin/contracts/access/Ownable2Step.sol";

/// @title World Chain Block Registry
/// @notice This contract records which blocks were built by the World Chain Builder.
/// @dev Each block, the builder will insert a transaction calling stampBlock() to indiciate which
/// blocks should enforce PBH priority ordering.
contract WorldChainBlockRegistry is Ownable2Step {
    ///////////////////////////////////////////////////////////////////////////////
    ///                             STATE VARIABLES                             ///
    //////////////////////////////////////////////////////////////////////////////

    address worldChainBuilder;
    mapping(uint256 blockNumber => address builder) public builtBlocks;

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  Events                                ///
    //////////////////////////////////////////////////////////////////////////////
    event BuiltBlock(address indexed builder, uint256 indexed blockNumber);
    event WorldChainBuilderUpdated(address indexed builder);

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  ERRORS                                ///
    //////////////////////////////////////////////////////////////////////////////
    error AddressZero();
    error BlockAlreadyRegistered();
    error Unauthorized();

    ///////////////////////////////////////////////////////////////////////////////
    ///                               MODIFIERS                                 ///
    ///////////////////////////////////////////////////////////////////////////////
    modifier onlyBuilder() {
        require(msg.sender == worldChainBuilder, Unauthorized());
        _;
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                               FUNCTIONS                                 ///
    ///////////////////////////////////////////////////////////////////////////////
    constructor(address builder) {
        require(builder != address(0), AddressZero());
        worldChainBuilder = builder;
    }

    function stampBlock() public onlyBuilder {
        require(builtBlocks[block.number] == address(0), BlockAlreadyRegistered());
        builtBlocks[block.number] = msg.sender;
        emit BuiltBlock(msg.sender, block.number);
    }

    function updateBuilder(address builder) public onlyOwner {
        require(builder != address(0), AddressZero());
        worldChainBuilder = builder;
        emit WorldChainBuilderUpdated(builder);
    }
}
