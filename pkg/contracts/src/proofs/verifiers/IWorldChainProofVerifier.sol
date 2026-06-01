// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title IWorldChainProofVerifier
/// @notice Generic, lane-agnostic verifier interface used by the proof-system
///         game. Every lane verifier binds the submitted material to `rootId`.
/// @dev Kept deliberately minimal so the game is impl-agnostic: the deploy
///      script can wire mocks today and real ZK/TEE/Council verifiers later
///      without changing the game.
interface IWorldChainProofVerifier {
    /// @notice Verifies that `proof` attests to `rootId` for this lane.
    /// @param rootId The WIP-1006 proof-bound commitment.
    /// @param proof Lane-specific, opaque proof material.
    /// @return ok Whether the proof is valid and binds to `rootId`.
    function verify(bytes32 rootId, bytes calldata proof) external view returns (bool ok);
}
