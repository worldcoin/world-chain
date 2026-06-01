// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IDisputeGame} from "./IDisputeGame.sol";
import {GameType, Hash} from "./Types.sol";

/// @title IAnchorStateRegistry
/// @notice ABI-matched reimplementation of the OP Stack `IAnchorStateRegistry`
///         interface, pinned to `op-contracts/v3.0.0-rc.2`.
interface IAnchorStateRegistry {
    /// @notice Returns whether a game's claim is considered valid by the registry.
    function isGameClaimValid(IDisputeGame _game) external view returns (bool);

    /// @notice Updates the anchor state from a finalized, valid game.
    function setAnchorState(IDisputeGame _game) external;

    /// @notice Returns the respected game type.
    function respectedGameType() external view returns (GameType);

    /// @notice Returns the anchor root for a game type.
    function anchors(GameType _gameType) external view returns (Hash, uint256);

    /// @notice Returns the current anchor root and its L2 block number.
    function getAnchorRoot() external view returns (Hash, uint256);

    /// @notice Returns whether a game is blacklisted.
    function isGameBlacklisted(IDisputeGame _game) external view returns (bool);

    /// @notice Returns whether a game is "proper" (created by the canonical factory).
    function isGameProper(IDisputeGame _game) external view returns (bool);

    /// @notice Returns whether a game is resolved.
    function isGameResolved(IDisputeGame _game) external view returns (bool);

    /// @notice Returns whether a game is of the respected game type.
    function isGameRespected(IDisputeGame _game) external view returns (bool);

    /// @notice Returns whether a game type is retired.
    function isGameRetired(IDisputeGame _game) external view returns (bool);

    /// @notice Returns whether a game is finalized.
    function isGameFinalized(IDisputeGame _game) external view returns (bool);
}
