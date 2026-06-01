// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IDisputeGame} from "../external/IDisputeGame.sol";

/// @title IWorldChainProofSystemGame
/// @notice World Chain extensions to the canonical OP `IDisputeGame` surface:
///         the WIP-1006 challenge/defense entrypoints and proof-state views.
interface IWorldChainProofSystemGame is IDisputeGame {
    /// @notice The WIP-1006 proof-bound commitment every lane MUST bind to.
    function rootId() external view returns (bytes32);

    /// @notice The parent reference (anchor registry or parent game).
    function parentRef() external view returns (address);

    /// @notice The per-root lane support bitmap.
    function proofBitmap() external view returns (uint8);

    /// @notice Distinct lane count supporting the root.
    function proofCount() external view returns (uint8);

    /// @notice The proof-window deadline for a challenged root.
    function proofDeadline() external view returns (uint64);

    /// @notice The challenge-window deadline.
    function challengeDeadline() external view returns (uint64);

    /// @notice Whether the root has been challenged.
    function challenged() external view returns (bool);

    /// @notice Challenges the proposed root (optimistic, no proof of invalidity).
    function challenge() external payable;

    /// @notice Submits lane support for a challenged root.
    function submitProofLane(uint8 laneId, bytes calldata proof) external;

    /// @notice Invalidates a challenged-but-undefended root.
    function invalidate() external returns (uint8 status_);

    /// @notice Claims the resolved proposer bond to the protocol-defined winner.
    function claimCredit() external;
}
