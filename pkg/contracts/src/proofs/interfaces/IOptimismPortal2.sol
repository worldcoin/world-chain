// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {GameType, Timestamp} from "../DisputeTypes.sol";
import {IDisputeGame} from "./IDisputeGame.sol";

/// @notice Factory surface called by OptimismPortal2 during withdrawal proving.
/// @dev Pinned to OptimismPortal2 5.6.1 from op-deployer v0.7.1 (optimism commit
///      7525482253bdc076548840638cde165c9004349f). This deliberately excludes the
///      factory administration and game-creation API that the Portal never calls.
interface IOptimismPortal2DisputeGameFactory {
    function gameAtIndex(uint256 index) external view returns (GameType, Timestamp, IDisputeGame);
}

/// @notice Anchor registry surface called by OptimismPortal2.
/// @dev OP declares game and factory parameters as contract interfaces. Their ABI encoding is
///      `address`, so this boundary uses addresses to match World Chain's internal registry API.
interface IOptimismPortal2AnchorStateRegistry {
    function disputeGameFactory() external view returns (address);
    function disputeGameFinalityDelaySeconds() external view returns (uint256);
    function respectedGameType() external view returns (GameType);
    function retirementTimestamp() external view returns (uint64);
    function disputeGameBlacklist(address game) external view returns (bool);
    function isGameProper(address game) external view returns (bool);
    function isGameRespected(address game) external view returns (bool);
    function isGameClaimValid(address game) external view returns (bool);
}
