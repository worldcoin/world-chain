// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

interface IWorldChainStakingRegistry {
    function isStaked(address account) external view returns (bool);
}
