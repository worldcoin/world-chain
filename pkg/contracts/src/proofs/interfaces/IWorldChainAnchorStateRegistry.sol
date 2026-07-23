// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {GameType, Hash, Proposal} from "../DisputeTypes.sol";

interface IWorldChainAnchorStateRegistry {
    function setAnchorState(address game) external;
    function isGameFinalized(address game) external view returns (bool);
    function isGameClaimValid(address game) external view returns (bool);
    function isGameProper(address game) external view returns (bool);
    function isGameRespected(address game) external view returns (bool);
    function isGameRegistered(address game) external view returns (bool);
    function disputeGameFactory() external view returns (address);
    function respectedGameType() external view returns (GameType);
    function disputeGameFinalityDelaySeconds() external view returns (uint256);
    function getAnchorRoot() external view returns (Hash, uint256);
    function getStartingAnchorRoot() external view returns (Proposal memory);
    function paused() external view returns (bool);
    function anchorGame() external view returns (address);
    function blacklistedGames(address game) external view returns (bool);
    function disputeGameBlacklist(address game) external view returns (bool);
    function isGameBlacklisted(address game) external view returns (bool);
    function retirementTimestamp() external view returns (uint64);
}
