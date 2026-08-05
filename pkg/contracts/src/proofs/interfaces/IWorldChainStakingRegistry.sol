// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title IWorldChainStakingRegistry
/// @author World Contributors
/// @custom:security-contact security@toolsforhumanity.com
interface IWorldChainStakingRegistry {
    function isStaked(address account) external view returns (bool);
}
