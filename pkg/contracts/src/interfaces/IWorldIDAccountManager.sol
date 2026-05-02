// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IWorldIDVerifier} from "./IWorldIDVerifier.sol";

/// @title IWorldIDAccountManager
/// @author Worldcoin
/// @notice Interface for the WIP-1001 World ID Account Manager.
///
///         A World ID Account is a predeploy-managed account whose lifetime-stable address is
///         `address(uint160(uint256(keccak256(worldIDAccountNullifier))))`. Creation is gated by a
///         World ID 4.0 Uniqueness Proof; subsequent mutations to the authenticator-key set are
///         gated by Session Proofs bound to a stored `sessionId`.
///
///         Authenticator keys are opaque byte strings: this contract is signature-algorithm
///         agnostic. The `signature_type` discriminator lives in the EIP-2718 `0x1D` envelope, not
///         on-chain. To minimize state, only `keccak256(key)` is persisted; the raw bytes are
///         emitted in events so off-chain indexers can reconstruct the active set.
///
/// @custom:security-contact security@toolsforhumanity.com
interface IWorldIDAccountManager {
    ///////////////////////////////////////////////////////////////////////////////
    ///                                  TYPES                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice The mutation operation a `WorldIDAccountUpdate` payload requests.
    enum Operation {
        Create,
        Update,
        Revert
    }

    /// @notice The signal payload bound into a World ID proof for an account action.
    /// @param operation Mutation kind. `Create` is only valid in `create`; `Update` only in `update`.
    /// @param addKeys Authenticator keys to install. Each key MUST be 1..MAX_KEY_BYTES bytes.
    /// @param removeKeys Authenticator keys to remove. Each key MUST be 1..MAX_KEY_BYTES bytes.
    struct WorldIDAccountUpdate {
        Operation operation;
        bytes[] addKeys;
        bytes[] removeKeys;
    }

    /// @notice Memory-resident scalar inputs for the verifier's `verifySession` call.
    /// @dev Used by the implementation to keep the call site's stack frame small enough to
    ///      compile without `via_ir`.
    struct SessionProofScalars {
        uint64 rpId;
        uint256 proofNonce;
        uint256 signalHash;
        uint64 expiresAtMin;
        uint64 issuerSchemaId;
        uint256 credentialGenesisIssuedAtMin;
        uint256 sessionId;
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                 EVENTS                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Emitted once when the implementation is initialized behind its proxy.
    event WorldIDAccountManagerImplInitialized(IWorldIDVerifier indexed worldIDVerifier, address indexed owner);

    /// @notice Emitted when the World ID verifier address is set or replaced.
    event WorldIDVerifierSet(address indexed worldIDVerifier);

    /// @notice Emitted when a World ID Account is first created.
    event WorldIDAccountCreated(
        address indexed worldIDAccount, uint256 indexed worldIDAccountNullifier, uint256 indexed sessionId
    );

    /// @notice Emitted when one or more authenticator keys are added to a World ID Account.
    /// @dev `keys` carries the raw authenticator bytes — on-chain state stores only their hashes.
    event SessionKeysAdded(address indexed worldIDAccount, bytes[] keys);

    /// @notice Emitted when one or more authenticator keys are removed from a World ID Account.
    /// @dev `keys` carries the raw authenticator bytes — on-chain state stores only their hashes.
    event SessionKeysRemoved(address indexed worldIDAccount, bytes[] keys);

    /// @notice Emitted when an authenticated key-set update is scheduled behind the cooldown.
    /// @dev The post-update key hashes are written into primary storage immediately, but these raw
    ///      keys are not yet effective until `validAfter`.
    event SessionKeyUpdateScheduled(
        address indexed worldIDAccount, bytes[] addKeys, bytes[] removeKeys, uint256 validAfter
    );

    /// @notice Emitted when a pending key-set update is reverted before it becomes effective.
    event SessionKeyUpdateReverted(address indexed worldIDAccount);

    /// @notice Emitted when the session-key update cooldown is set or replaced.
    event SessionKeyUpdateCooldownSet(uint256 cooldown);

    ///////////////////////////////////////////////////////////////////////////////
    ///                                 ERRORS                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Thrown when the operation in a payload is not valid for the entry point used.
    error InvalidOperation();

    /// @notice Thrown when `create` targets a World ID Account that already exists.
    error WorldIDAccountAlreadyExists();

    /// @notice Thrown when `create` is invoked with a zero `worldIDAccountNullifier_`.
    /// @dev A zero nullifier would be persisted indistinguishably from "absent", letting the
    ///      same account be created repeatedly and permanently breaking `update`.
    error ZeroNullifier();

    /// @notice Thrown when `update` targets a World ID Account that has not been created.
    error WorldIDAccountDoesNotExist();

    /// @notice Thrown when a payload contains no keys.
    error EmptyKeySet();

    /// @notice Thrown when a payload would result in more than `MAX_SESSION_KEYS` authorized keys.
    error TooManyKeys(uint256 actual, uint256 max);

    /// @notice Thrown when a key in the payload has zero length.
    error EmptyKey();

    /// @notice Thrown when a key exceeds `MAX_KEY_BYTES`.
    error KeyTooLarge(uint256 actual, uint256 max);

    /// @notice Thrown when an add payload includes a key already authorized for the account.
    error DuplicateSessionKey(bytes32 keyHash);

    /// @notice Thrown when a remove payload includes a key not authorized for the account.
    error UnknownSessionKey(bytes32 keyHash);

    /// @notice Thrown when the same key appears in both `addKeys` and `removeKeys`.
    error OverlappingUpdateKey(bytes32 keyHash);

    /// @notice Thrown when a required address parameter is the zero address.
    error AddressZero();

    /// @notice Thrown when the configured session-key update cooldown is zero.
    error ZeroCooldown();

    /// @notice Thrown when `update` is attempted while another update is still pending.
    error PendingSessionKeyUpdate(uint256 validAfter);

    /// @notice Thrown when `revert` is called but no pending update exists.
    error NoPendingSessionKeyUpdate();

    /// @notice Thrown when `revert` is called after the pending update's window elapsed.
    error PendingSessionKeyUpdateExpired(uint256 validAfter);

    ///////////////////////////////////////////////////////////////////////////////
    ///                              EXTERNAL API                               ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Returns the configured cooldown for pending key-set updates.
    function sessionKeyUpdateCooldown() external view returns (uint256);

    /// @notice Create a World ID Account authorized by a World ID 4.0 Uniqueness Proof.
    /// @param worldIDAccountNullifier_ The creation nullifier `ν_k`.
    /// @param worldIDAccountNonce_ User-chosen account selector folded into the action.
    /// @param proofNonce_ Verifier request nonce for the World ID 4.0 proof.
    /// @param sessionId_ Fresh session identifier to associate with the new account.
    /// @param issuerSchemaId_ Credential schema/issuer identifier.
    /// @param expiresAtMin_ Minimum credential expiration constraint.
    /// @param credentialGenesisIssuedAtMin_ Minimum credential `genesis_issued_at`.
    /// @param proof_ Compressed Groth16 proof `[a, b0, b1, c, merkle_root]`.
    /// @param createUpdate_ Initial set of authenticator keys to install. `removeKeys` MUST be empty.
    /// @return worldIDAccount_ The deterministic address of the newly created account.
    function create(
        uint256 worldIDAccountNullifier_,
        uint256 worldIDAccountNonce_,
        uint256 proofNonce_,
        uint256 sessionId_,
        uint64 issuerSchemaId_,
        uint64 expiresAtMin_,
        uint256 credentialGenesisIssuedAtMin_,
        uint256[5] calldata proof_,
        WorldIDAccountUpdate calldata createUpdate_
    ) external returns (address worldIDAccount_);

    /// @notice Apply an update authorized by a World ID 4.0 Session Proof.
    /// @param worldIDAccount_ The address of the World ID Account being updated.
    /// @param proofNonce_ Verifier request nonce for the Session Proof.
    /// @param issuerSchemaId_ Credential schema/issuer identifier.
    /// @param expiresAtMin_ Minimum credential expiration constraint.
    /// @param credentialGenesisIssuedAtMin_ Minimum credential `genesis_issued_at`.
    /// @param sessionNullifier_ Per-update verifier input `[nullifier, randomAction]`. Ephemeral.
    /// @param proof_ Compressed Groth16 proof `[a, b0, b1, c, merkle_root]`.
    /// @param accountUpdate_ The add/remove payload. At least one of `addKeys` or `removeKeys`
    ///        MUST be non-empty.
    function update(
        address worldIDAccount_,
        uint256 proofNonce_,
        uint64 issuerSchemaId_,
        uint64 expiresAtMin_,
        uint256 credentialGenesisIssuedAtMin_,
        uint256[2] calldata sessionNullifier_,
        uint256[5] calldata proof_,
        WorldIDAccountUpdate calldata accountUpdate_
    ) external;

    /// @notice Revert a pending key-set update authorized by a World ID 4.0 Session Proof.
    /// @dev Reverts the pending window and restores the previous effective key set into primary
    ///      storage. Reverts if no pending update exists or if the pending window already elapsed.
    function revert(
        address worldIDAccount_,
        uint256 proofNonce_,
        uint64 issuerSchemaId_,
        uint64 expiresAtMin_,
        uint256 credentialGenesisIssuedAtMin_,
        uint256[2] calldata sessionNullifier_,
        uint256[5] calldata proof_
    ) external;

    /// @notice Returns the hashes of the currently effective authenticator keys for `worldIDAccount_`.
    /// @dev During an active pending update this returns the previous key set, not the newly stored
    ///      one. Raw key bytes are not stored on-chain.
    function getSessionKeyHashes(address worldIDAccount_) external view returns (bytes32[] memory);

    /// @notice Returns the hashes written into primary storage for `worldIDAccount_`.
    /// @dev During an active pending update this is the future post-cooldown key set.
    function getStoredSessionKeyHashes(address worldIDAccount_) external view returns (bytes32[] memory);

    /// @notice Returns the previous key-set snapshot for an active pending update.
    /// @dev Returns an empty array once the pending update is no longer active.
    function getPreviousSessionKeyHashes(address worldIDAccount_) external view returns (bytes32[] memory);

    /// @notice Returns the timestamp at which a pending update becomes effective.
    /// @dev Returns `0` when there is no active pending update.
    function getPendingValidAfter(address worldIDAccount_) external view returns (uint256);

    /// @notice Returns whether `key_` is currently authorized for `worldIDAccount_`.
    /// @dev Returns `false` (not revert) for malformed key lengths.
    function isAuthorized(address worldIDAccount_, bytes calldata key_) external view returns (bool);

    /// @notice Updates the cooldown used for future key-set updates.
    /// @param sessionKeyUpdateCooldown_ The new cooldown in seconds. Must be non-zero.
    function setSessionKeyUpdateCooldown(uint256 sessionKeyUpdateCooldown_) external;
}
