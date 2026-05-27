// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {IStakingRegistry} from "../../../src/proofs/interfaces/IStakingRegistry.sol";

contract MockStakingRegistry is IStakingRegistry {
    mapping(address challenger => bool staked) public staked;
    mapping(bytes32 rootId => mapping(address challenger => uint256 amount)) public lockedBonds;
    mapping(bytes32 rootId => mapping(address challenger => uint256 amount)) public forfeitedBonds;
    mapping(bytes32 rootId => mapping(address challenger => uint256 amount)) public refundedBonds;

    function setStaked(address challenger, bool isChallengerStaked) external {
        staked[challenger] = isChallengerStaked;
    }

    function isStaked(address challenger) external view returns (bool) {
        return staked[challenger];
    }

    function lockChallengeBond(address challenger, bytes32 rootId, uint256 amount) external {
        lockedBonds[rootId][challenger] += amount;
    }

    function forfeitChallengeBond(address challenger, bytes32 rootId) external {
        uint256 amount = lockedBonds[rootId][challenger];
        lockedBonds[rootId][challenger] = 0;
        forfeitedBonds[rootId][challenger] += amount;
    }

    function refundChallengeBond(address challenger, bytes32 rootId) external {
        uint256 amount = lockedBonds[rootId][challenger];
        lockedBonds[rootId][challenger] = 0;
        refundedBonds[rootId][challenger] += amount;
    }
}
