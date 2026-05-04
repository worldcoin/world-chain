// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @title IWorldIDVerifier
/// @author World Contributors
/// @notice Minimal interface for the World ID 4.0 verifier.
/// @dev Signatures are aligned byte-for-byte with
///      `world-id-protocol/contracts/src/core/interfaces/IWorldIDVerifier.sol` so the deployed
///      verifier can be invoked through this interface without ABI mismatch. Only the two methods
///      consumed by this package are declared; admin and view-helpers live on the concrete contract.
interface IWorldIDVerifier {
    /// @notice Verifies a Uniqueness Proof.
    /// @param nullifier Public output. Unique one-time identifier derived from (user, rpId, action).
    /// @param action Public input. RP-defined context (already reduced to the BN254 scalar field).
    /// @param rpId Public input. Registered RP identifier from the `RpRegistry`.
    /// @param nonce Public input. Per-request nonce provided by the RP.
    /// @param signalHash Public input. Hash of arbitrary RP data bound into the proof.
    /// @param expiresAtMin Public input. Minimum credential expiration the proof asserts.
    /// @param issuerSchemaId Public input. Credential schema/issuer identifier.
    /// @param credentialGenesisIssuedAtMin Public input. Minimum credential `genesis_issued_at`.
    /// @param zeroKnowledgeProof Compressed Groth16 proof `[a, b0, b1, c, merkle_root]`.
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

    /// @notice Verifies a Session Proof.
    /// @param rpId Public input. Registered RP identifier from the `RpRegistry`.
    /// @param nonce Public input. Per-request nonce provided by the RP.
    /// @param signalHash Public input. Hash of arbitrary RP data bound into the proof.
    /// @param expiresAtMin Public input. Minimum credential expiration the proof asserts.
    /// @param issuerSchemaId Public input. Credential schema/issuer identifier.
    /// @param credentialGenesisIssuedAtMin Public input. Minimum credential `genesis_issued_at`.
    /// @param sessionId Public input. Session identifier connecting proofs for the same user+RP pair.
    /// @param sessionNullifier `[nullifier, randomAction]` per-update verifier input. Ephemeral.
    /// @param zeroKnowledgeProof Compressed Groth16 proof `[a, b0, b1, c, merkle_root]`.
    function verifySession(
        uint64 rpId,
        uint256 nonce,
        uint256 signalHash,
        uint64 expiresAtMin,
        uint64 issuerSchemaId,
        uint256 credentialGenesisIssuedAtMin,
        uint256 sessionId,
        uint256[2] calldata sessionNullifier,
        uint256[5] calldata zeroKnowledgeProof
    ) external view;
}
