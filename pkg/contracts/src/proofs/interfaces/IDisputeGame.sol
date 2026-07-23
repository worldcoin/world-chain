// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Claim, GameStatus, GameType, Hash, Timestamp} from "../DisputeTypes.sol";

/// @notice Game read surface consumed by OptimismPortal2 and WorldChainAnchorStateRegistry.
/// @dev Pinned to OptimismPortal2 5.6.1 from op-deployer v0.7.1 (optimism commit
///      7525482253bdc076548840638cde165c9004349f). This is not the complete OP Stack
///      IDisputeGame lifecycle interface: in particular, World Chain's resolve() returns WIP-1006
///      state and invalidation data, while the OP lifecycle method returns only GameStatus.
///      Timestamp and Claim are expressed as their uint64/bytes32 ABI types where needed so the
///      concrete game's public getters can explicitly implement this interface.
interface IDisputeGame {
    function createdAt() external view returns (uint64);
    function resolvedAt() external view returns (Timestamp);
    function status() external view returns (GameStatus);
    function gameType() external view returns (GameType);
    function gameCreator() external view returns (address);
    function rootClaim() external view returns (bytes32);
    function rootClaimByChainId(uint256 chainId) external view returns (Claim);
    function l1Head() external view returns (Hash);
    function l2SequenceNumber() external view returns (uint256);
    function extraData() external view returns (bytes memory);
    function gameData() external view returns (GameType, Claim, bytes memory);
    function wasRespectedGameTypeWhenCreated() external view returns (bool);
}
