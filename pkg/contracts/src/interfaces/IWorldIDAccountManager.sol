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
        Add,
        Remove
    }

    /// @notice The signal payload bound into a World ID proof for an account action.
    /// @param operation Mutation kind. `Create` is only valid in `create`; `Add`/`Remove` only in `update`.
    /// @param keys Authenticator keys to install or remove. Each key MUST be 1..MAX_KEY_BYTES bytes.
    struct WorldIDAccountUpdate {
        Operation operation;
        bytes[] keys;
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

    /// @notice Thrown when a payload contains zero keys.
    error EmptyKeySet();

    /// @notice Thrown when a payload would result in more than `MAX_SESSION_KEYS` authorized keys.
    error TooManyKeys(uint256 actual, uint256 max);

    /// @notice Thrown when a key in the payload has zero length.
    error EmptyKey();

    /// @notice Thrown when a key exceeds `MAX_KEY_BYTES`.
    error KeyTooLarge(uint256 actual, uint256 max);

    /// @notice Thrown when an `Add` payload includes a key already authorized for the account.
    error DuplicateSessionKey(bytes32 keyHash);

    /// @notice Thrown when a `Remove` payload includes a key not authorized for the account.
    error UnknownSessionKey(bytes32 keyHash);

    /// @notice Thrown when a required address parameter is the zero address.
    error AddressZero();

    ///////////////////////////////////////////////////////////////////////////////
    ///                              EXTERNAL API                               ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Create a World ID Account authorized by a World ID 4.0 Uniqueness Proof.
    /// @param worldIDAccountNullifier_ The creation nullifier `ν_k`.
    /// @param worldIDAccountNonce_ User-chosen account selector folded into the action.
    /// @param proofNonce_ Verifier request nonce for the World ID 4.0 proof.
    /// @param sessionId_ Fresh session identifier to associate with the new account.
    /// @param issuerSchemaId_ Credential schema/issuer identifier.
    /// @param expiresAtMin_ Minimum credential expiration constraint.
    /// @param credentialGenesisIssuedAtMin_ Minimum credential `genesis_issued_at`.
    /// @param proof_ Compressed Groth16 proof `[a, b0, b1, c, merkle_root]`.
    /// @param createUpdate_ Initial set of authenticator keys to install.
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

    /// @notice Apply an `Add` or `Remove` update authorized by a World ID 4.0 Session Proof.
    /// @param worldIDAccount_ The address of the World ID Account being updated.
    /// @param proofNonce_ Verifier request nonce for the Session Proof.
    /// @param issuerSchemaId_ Credential schema/issuer identifier.
    /// @param expiresAtMin_ Minimum credential expiration constraint.
    /// @param credentialGenesisIssuedAtMin_ Minimum credential `genesis_issued_at`.
    /// @param sessionNullifier_ Per-update verifier input `[nullifier, randomAction]`. Ephemeral.
    /// @param proof_ Compressed Groth16 proof `[a, b0, b1, c, merkle_root]`.
    /// @param accountUpdate_ The Add/Remove payload.
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

    /// @notice Returns the hashes of all currently authorized authenticator keys for `worldIDAccount_`.
    /// @dev Raw key bytes are not stored on-chain; reconstruct from `SessionKeysAdded` /
    ///      `SessionKeysRemoved` events if needed.
    function getSessionKeyHashes(address worldIDAccount_) external view returns (bytes32[] memory);

    /// @notice Returns whether `key_` is currently authorized for `worldIDAccount_`.
    /// @dev Returns `false` (not revert) for malformed key lengths.
    function isAuthorized(address worldIDAccount_, bytes calldata key_) external view returns (bool);
}
