// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @title IWorldIDVerifier
/// @author Worldcoin
/// @notice Interface to the on-chain World ID v4 OPRF verifier.
/// @dev This is NOT the existing Semaphore verifier used in PBH.
///      It verifies zero-knowledge proofs produced by the World ID v4 OPRF scheme.
/// @custom:security-contact security@toolsforhumanity.com
interface IWorldIDVerifier {
    /// @notice Verifies a World ID v4 OPRF zero-knowledge proof.
    /// @param nullifier The nullifier derived from the proof.
    /// @param action The action identifier (hashed action string).
    /// @param rpId The relying party identifier (e.g., 480 for World Chain).
    /// @param nonce A nonce binding the proof to a specific context.
    /// @param signalHash A hash of the signal being proven.
    /// @param expiresAtMin The minimum expiration timestamp (in minutes).
    /// @param issuerSchemaId The schema identifier for the credential issuer.
    /// @param credentialGenesisIssuedAtMin The minimum issuance timestamp for genesis credentials.
    /// @param zeroKnowledgeProof The five-element ZK proof array.
    function verify(
        uint256 nullifier,
        uint256 action,
        uint64 rpId,
        uint256 nonce,
        uint256 signalHash,
        uint64 expiresAtMin,
        uint64 issuerSchemaId,
        uint256 credentialGenesisIssuedAtMin,
        uint256[5] calldata zeroKnowledgeProof
    ) external view;
}
