// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

interface IWorldChainBlockRegistry {
    function stampBlock() external;
    function updateBuilder(address builder) external;
}
