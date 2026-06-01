// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IDisputeGame} from "./IDisputeGame.sol";
import {GameType, Claim, Hash, Timestamp} from "./Types.sol";

/// @title IDisputeGameFactory
/// @notice ABI-matched reimplementation of the OP Stack `IDisputeGameFactory`
///         interface, pinned to `op-contracts/v3.0.0-rc.2`.
interface IDisputeGameFactory {
    /// @notice Creates a new DisputeGame proxy contract.
    /// @param _gameType The type of the DisputeGame - used to decide the proxy implementation.
    /// @param _rootClaim The root claim of the DisputeGame.
    /// @param _extraData Any extra data that should be provided to the created dispute game.
    /// @return proxy_ The address of the created DisputeGame proxy.
    function create(GameType _gameType, Claim _rootClaim, bytes calldata _extraData)
        external
        payable
        returns (IDisputeGame proxy_);

    /// @notice Sets the implementation contract for a specific `GameType`.
    function setImplementation(GameType _gameType, IDisputeGame _impl) external;

    /// @notice Sets the bond (in wei) for initializing a game type.
    function setInitBond(GameType _gameType, uint256 _initBond) external;

    /// @notice Returns the required bonds for initializing a dispute game of the given type.
    function initBonds(GameType _gameType) external view returns (uint256);

    /// @notice Returns the implementation contract for a specific `GameType`.
    function gameImpls(GameType _gameType) external view returns (IDisputeGame);

    /// @notice Queries the deployed `IDisputeGame` clone for the given parameters.
    function games(GameType _gameType, Claim _rootClaim, bytes calldata _extraData)
        external
        view
        returns (IDisputeGame proxy_, Timestamp timestamp_);

    /// @notice Returns a unique identifier for the given dispute game parameters.
    function getGameUUID(GameType _gameType, Claim _rootClaim, bytes calldata _extraData)
        external
        pure
        returns (Hash uuid_);
}
