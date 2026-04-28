// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @title IWorldIDKeyRing
/// @notice Interface for the WIP-1001 World ID Key Ring stateful precompile.
///
/// A keyring is a precompile-managed account whose lifetime-stable address is
/// `bytes20(keccak256(keyringNullifier))`. Creation is gated by a World ID 4.0
/// Uniqueness Proof; subsequent mutations are gated by Session Proofs bound to
/// a stored `sessionId`.
interface IWorldIDKeyRing {
    /// @notice Authenticator key family.
    /// @dev `keyData` length is determined by the key type:
    ///        - Secp256k1: 33 bytes (compressed)
    ///        - P256:      64 bytes (uncompressed `x || y`)
    ///        - WebAuthn:  64 bytes (P-256 `x || y` under WebAuthn)
    ///        - EdDSA:     32 bytes (compressed Ed25519)
    enum KeyType {
        Secp256k1,
        P256,
        WebAuthn,
        EdDSA
    }

    /// @notice Mode for a `KeyringUpdate` payload.
    enum UpdateMode {
        Create,
        Add,
        Remove
    }

    /// @notice A session key authorized to sign `0x1D` transactions for a keyring.
    struct SessionKey {
        KeyType keyType;
        bytes keyData;
    }

    /// @notice The signal payload bound into the World ID proof for a keyring action.
    struct KeyringUpdate {
        UpdateMode mode;
        SessionKey[] keys;
    }

    /// @notice Emitted when a keyring is first created.
    event KeyringCreated(
        address indexed keyring, uint256 indexed keyringNullifier, uint256 indexed sessionId
    );

    /// @notice Emitted when one or more session keys are added to a keyring.
    event SessionKeysAdded(address indexed keyring, SessionKey[] keys);

    /// @notice Emitted when one or more session keys are removed from a keyring.
    event SessionKeysRemoved(address indexed keyring, SessionKey[] keys);

    error InvalidUpdateMode();
    error KeyringAlreadyExists();
    error KeyringDoesNotExist();
    error EmptyKeySet();
    error TooManyKeys();
    error DuplicateSessionKey();
    error UnknownSessionKey();
    error InvalidKeyDataLength(uint8 keyType, uint256 actual);

    /// @notice Create a keyring authorized by a World ID 4.0 Uniqueness Proof.
    /// @dev The precompile MUST:
    ///      1. Require `createUpdate.mode == Create`.
    ///      2. Recompute `action = keccak256(WORLD_ID_KEYRING_TAG || keyringNonce)`.
    ///      3. Recompute `signalHash = uint256(keccak256(abi.encode(createUpdate))) >> 8`.
    ///      4. Invoke `WorldID.verify(...)`.
    ///      5. Derive the keyring address from `keyringNullifier`, associate the
    ///         supplied `sessionId`, and apply `createUpdate` immediately.
    /// @param keyringNullifier The creation nullifier `ν_k`.
    /// @param keyringNonce User-chosen keyring selector folded into the action.
    /// @param proofNonce Verifier request nonce for the World ID 4.0 proof.
    /// @param sessionId Fresh `sessionId` to associate with the new keyring.
    /// @param issuerSchemaId Credential schema/issuer identifier.
    /// @param expiresAtMin Minimum credential expiration constraint.
    /// @param credentialGenesisIssuedAtMin Minimum credential `genesis_issued_at`.
    /// @param proof Compressed Groth16 proof `[a, b0, b1, c, merkle_root]`.
    /// @param createUpdate Initial set of session keys to install.
    /// @return keyringAddress The deterministic address of the newly created keyring.
    function create(
        uint256 keyringNullifier,
        uint256 keyringNonce,
        uint256 proofNonce,
        uint256 sessionId,
        uint64 issuerSchemaId,
        uint64 expiresAtMin,
        uint256 credentialGenesisIssuedAtMin,
        uint256[5] calldata proof,
        KeyringUpdate calldata createUpdate
    ) external returns (address keyringAddress);

    /// @notice Apply an `Add` or `Remove` update authorized by a World ID 4.0 Session Proof.
    /// @dev The precompile MUST:
    ///      1. Require `keyringUpdate.mode != Create`.
    ///      2. Load the stored `sessionId` for `keyring`.
    ///      3. Recompute `signalHash = uint256(keccak256(abi.encode(keyringUpdate, generation))) >> 8`
    ///         where `generation` is a per-keyring monotonic counter so identical
    ///         payloads still yield distinct signals.
    ///      4. Invoke `WorldID.verifySession(..., sessionId, sessionNullifier, proof)`.
    ///      5. Discard `sessionNullifier` after verification (MUST NOT be persisted).
    ///      6. Apply the update (queued behind the timelock once timelock is in scope).
    /// @param keyring The address of the keyring being updated.
    /// @param proofNonce Verifier request nonce for the Session Proof.
    /// @param issuerSchemaId Credential schema/issuer identifier.
    /// @param expiresAtMin Minimum credential expiration constraint.
    /// @param credentialGenesisIssuedAtMin Minimum credential `genesis_issued_at`.
    /// @param sessionNullifier Per-update verifier input `[nullifier, randomAction]`.
    /// @param proof Compressed Groth16 proof `[a, b0, b1, c, merkle_root]`.
    /// @param keyringUpdate The Add/Remove payload.
    function update(
        address keyring,
        uint256 proofNonce,
        uint64 issuerSchemaId,
        uint64 expiresAtMin,
        uint256 credentialGenesisIssuedAtMin,
        uint256[2] calldata sessionNullifier,
        uint256[5] calldata proof,
        KeyringUpdate calldata keyringUpdate
    ) external;

    /// @notice Returns the current set of session keys authorized for `keyring`.
    function getSessionKeys(address keyring) external view returns (SessionKey[] memory);

    /// @notice Returns whether `key` is currently authorized for `keyring`.
    function isAuthorized(address keyring, SessionKey calldata key) external view returns (bool);

    /// @notice Returns the creation nullifier `ν_k` for `keyring` (zero if unknown).
    function getKeyringNullifier(address keyring) external view returns (uint256);
}