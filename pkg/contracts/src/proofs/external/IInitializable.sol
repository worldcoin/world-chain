// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title IInitializable
/// @notice ABI-matched reimplementation of the OP Stack `IInitializable`
///         interface, pinned to `op-contracts/v3.0.0-rc.2`.
interface IInitializable {
    /// @notice Initializes the contract.
    /// @dev Called by the `DisputeGameFactory` immediately after cloning the implementation.
    function initialize() external payable;
}
