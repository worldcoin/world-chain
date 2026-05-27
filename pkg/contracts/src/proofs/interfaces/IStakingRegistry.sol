// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

interface IStakingRegistry {
    function isStaked(address challenger) external view returns (bool);
    function lockChallengeBond(address challenger, bytes32 rootId, uint256 amount) external;
    function forfeitChallengeBond(address challenger, bytes32 rootId) external;
    function refundChallengeBond(address challenger, bytes32 rootId) external;
}
