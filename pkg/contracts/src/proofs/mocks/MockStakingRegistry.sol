// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainStakingRegistry} from "../interfaces/IWorldChainStakingRegistry.sol";

contract MockStakingRegistry is IWorldChainStakingRegistry {
    mapping(address account => bool staked) public staked;

    function setStaked(address account, bool staked_) external {
        staked[account] = staked_;
    }

    function stake() external payable {
        staked[msg.sender] = true;
    }

    function isStaked(address account) external view returns (bool) {
        return staked[account];
    }
}
