// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IInitializable} from "./IInitializable.sol";
import {Timestamp, GameStatus, GameType, Claim, Hash} from "./Types.sol";

/// @title IDisputeGame
/// @notice ABI-matched reimplementation of the OP Stack `IDisputeGame`
///         interface, pinned to `op-contracts/v3.0.0-rc.2`.
interface IDisputeGame is IInitializable {
    /// @notice Emitted when the game is resolved.
    /// @param status The status of the game after resolution.
    event Resolved(GameStatus indexed status);

    /// @notice Returns the timestamp that the DisputeGame contract was created at.
    function createdAt() external view returns (Timestamp);

    /// @notice Returns the timestamp that the DisputeGame contract was resolved at.
    function resolvedAt() external view returns (Timestamp);

    /// @notice Returns the current status of the game.
    function status() external view returns (GameStatus);

    /// @notice Getter for the game type.
    function gameType() external view returns (GameType gameType_);

    /// @notice Getter for the creator of the dispute game.
    function gameCreator() external pure returns (address creator_);

    /// @notice Getter for the root claim.
    function rootClaim() external pure returns (Claim rootClaim_);

    /// @notice Getter for the parent hash of the L1 block when the dispute game was created.
    function l1Head() external pure returns (Hash l1Head_);

    /// @notice Getter for the extra data.
    function l2BlockNumber() external pure returns (uint256 l2BlockNumber_);

    /// @notice Getter for the extra data.
    function extraData() external pure returns (bytes memory extraData_);

    /// @notice If all necessary information has been gathered, this function should mark the game
    ///         status as either `CHALLENGER_WINS` or `DEFENDER_WINS` and return the status of
    ///         the resolved game.
    function resolve() external returns (GameStatus status_);

    /// @notice A compliant implementation of this interface should return the components of the
    ///         game UUID's preimage provided in the cwia payload.
    function gameData() external view returns (GameType gameType_, Claim rootClaim_, bytes memory extraData_);

    /// @notice Returns whether the game type was the respected game type when the game was created.
    function wasRespectedGameTypeWhenCreated() external view returns (bool);
}
