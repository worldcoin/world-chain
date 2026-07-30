// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainStakingRegistry} from "../../src/proofs/interfaces/IWorldChainStakingRegistry.sol";

/// @dev Test-only stand-in for the challenger staking gate. It has no minimum stake, no slashing,
///      and unauthenticated mutators, so it deliberately lives under `test/` where it cannot be
///      wired into a production `GameConfig`. A real `IWorldChainStakingRegistry` does not exist
///      yet; see the WIP-1006 deployment checklist.
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
