// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title IWorldChainProofVerifier
/// @author World Contributors
/// @custom:security-contact security@toolsforhumanity.com
interface IWorldChainProofVerifier {
    /// @notice Verifies `proof` against a proof-system identity and its expected public values.
    /// @param proof Lane-specific proof bytes.
    /// @param verifierId Immutable identity selected by the game, such as a program vkey or TEE image ID.
    /// @param publicValues Exact public values the proof must authenticate.
    function verify(bytes calldata proof, bytes32 verifierId, bytes calldata publicValues) external view returns (bool);
}
