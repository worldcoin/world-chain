// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {Base} from "./abstract/Base.sol";
import {IWorldIDAccountManager} from "./interfaces/IWorldIDAccountManager.sol";
import {IWorldIDVerifier} from "./interfaces/IWorldIDVerifier.sol";

/// @title World ID Account Manager Implementation V1
/// @author Worldcoin
/// @notice Solidity stand-in for the WIP-1001 `WORLD_ID_ACCOUNT_PRECOMPILE`. Manages the
///         authenticator-key set for each World ID Account, gated by World ID 4.0 proofs.
/// @dev All upgrades to `WorldIDAccountManager` after initial deployment must inherit this
///      contract to avoid storage collisions. Storage variables MUST NOT be reordered after
///      deployment otherwise storage collisions will occur.
/// @custom:security-contact security@toolsforhumanity.com
contract WorldIDAccountManagerImplV1 is IWorldIDAccountManager, Base, ReentrancyGuardTransient {
    ///////////////////////////////////////////////////////////////////////////////
    ///                                CONSTANTS                                ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice World Chain's registered RP ID (per WIP-1001).
    uint64 public constant WORLD_CHAIN_RP_ID = 480;

    /// @notice Domain tag folded into the action for World ID Account Uniqueness Proofs.
    bytes16 public constant WORLD_ID_ACCOUNT_TAG = "WORLD_ID_ACCOUNT";

    /// @notice Domain tag used to separate revert proofs from key-set update proofs.
    bytes16 public constant WORLD_ID_ACCOUNT_REVERT_TAG = "WORLD_ID_REVERT";

    /// @notice Maximum authorized authenticator keys per World ID Account.
    uint256 public constant MAX_SESSION_KEYS = 20;

    /// @notice Maximum byte length of a single authenticator key.
    uint256 public constant MAX_KEY_BYTES = 128;

    ///////////////////////////////////////////////////////////////////////////////
    ///                             STATE VARIABLES                             ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice The World ID verifier proxy address.
    IWorldIDVerifier public worldIDVerifier;

    /// @inheritdoc IWorldIDAccountManager
    uint256 public sessionKeyUpdateCooldown;

    /// @notice A map from World ID accounts to their single use creation nullifier.
    mapping(address worldIDAccount => uint256 nullifier) public worldIDAccountNullifier;

    /// @notice The session identifier installed at creation, used to scope subsequent Session Proofs.
    mapping(address worldIDAccount => uint256 sessionId) public sessionIdOf;

    /// @notice Per-account monotonic counter folded into update and revert signal hashes.
    mapping(address worldIDAccount => uint256 generation) public generationOf;

    /// @notice Hashes of the post-cooldown authenticator keys for each account.
    mapping(address worldIDAccount => bytes32[] keyHashes) public sessionKeyHashes;

    /// @notice 1-indexed position in `sessionKeyHashes[acct]` of each key hash. `0` means absent.
    mapping(address worldIDAccount => mapping(bytes32 keyHash => uint256 indexPlusOne)) public keyHashIndex;

    /// @notice Hashes of the effective pre-update authenticator keys during an active pending window.
    mapping(address worldIDAccount => bytes32[] keyHashes) public previousSessionKeyHashes;

    /// @notice 1-indexed position in `previousSessionKeyHashes[acct]` of each key hash.
    mapping(address worldIDAccount => mapping(bytes32 keyHash => uint256 indexPlusOne)) public previousKeyHashIndex;

    /// @notice Timestamp at which the stored key set becomes effective. `0` means no pending update.
    mapping(address worldIDAccount => uint256 validAfter) public pendingValidAfter;

    ///////////////////////////////////////////////////////////////////////////////
    ///                              CONSTRUCTION                               ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Locks the implementation. State setup happens in `initialize` against the proxy.
    constructor() {
        _disableInitializers();
    }

    /// @notice Initializes the proxy.
    /// @param worldIDVerifier_ The World ID 4.0 verifier address. Must not be zero.
    /// @param owner_ The owner that will gain admin privileges via `Ownable2Step`.
    /// @param sessionKeyUpdateCooldown_ The delay applied to future key-set updates. Must be non-zero.
    function initialize(IWorldIDVerifier worldIDVerifier_, address owner_, uint256 sessionKeyUpdateCooldown_)
        external
        reinitializer(1)
    {
        if (address(worldIDVerifier_) == address(0)) revert AddressZero();
        if (owner_ == address(0)) revert AddressZero();
        if (sessionKeyUpdateCooldown_ == 0) revert ZeroCooldown();

        __Base_init(owner_);
        worldIDVerifier = worldIDVerifier_;
        sessionKeyUpdateCooldown = sessionKeyUpdateCooldown_;

        emit WorldIDAccountManagerImplInitialized(worldIDVerifier_, owner_);
        emit SessionKeyUpdateCooldownSet(sessionKeyUpdateCooldown_);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              EXTERNAL API                               ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @inheritdoc IWorldIDAccountManager
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
    ) external virtual onlyProxy nonReentrant returns (address worldIDAccount_) {
        if (createUpdate_.operation != Operation.Create) revert InvalidOperation();
        // A zero nullifier would be indistinguishable from "absent" in `worldIDAccountNullifier`,
        // letting the same account be re-created and permanently bricking future updates.
        if (worldIDAccountNullifier_ == 0) revert ZeroNullifier();

        worldIDAccount_ = address(uint160(uint256(keccak256(abi.encodePacked(worldIDAccountNullifier_)))));
        if (worldIDAccountNullifier[worldIDAccount_] != 0) revert WorldIDAccountAlreadyExists();
        if (createUpdate_.removeKeys.length != 0) revert InvalidOperation();

        uint256 action = uint256(keccak256(abi.encodePacked(WORLD_ID_ACCOUNT_TAG, worldIDAccountNonce_))) >> 8;
        uint256 signalHash = uint256(keccak256(abi.encode(createUpdate_))) >> 8;

        worldIDVerifier.verify(
            worldIDAccountNullifier_,
            action,
            WORLD_CHAIN_RP_ID,
            proofNonce_,
            signalHash,
            expiresAtMin_,
            issuerSchemaId_,
            credentialGenesisIssuedAtMin_,
            proof_
        );

        worldIDAccountNullifier[worldIDAccount_] = worldIDAccountNullifier_;
        sessionIdOf[worldIDAccount_] = sessionId_;

        _addKeys(worldIDAccount_, createUpdate_.addKeys);

        emit WorldIDAccountCreated(worldIDAccount_, worldIDAccountNullifier_, sessionId_);
        emit SessionKeysAdded(worldIDAccount_, createUpdate_.addKeys);
    }

    /// @inheritdoc IWorldIDAccountManager
    function update(
        address worldIDAccount_,
        uint256 proofNonce_,
        uint64 issuerSchemaId_,
        uint64 expiresAtMin_,
        uint256 credentialGenesisIssuedAtMin_,
        uint256[2] calldata sessionNullifier_,
        uint256[5] calldata proof_,
        WorldIDAccountUpdate calldata accountUpdate_
    ) external virtual onlyProxy nonReentrant {
        if (accountUpdate_.operation != Operation.Update) {
            revert InvalidOperation();
        }
        if (worldIDAccountNullifier[worldIDAccount_] == 0) revert WorldIDAccountDoesNotExist();
        if (accountUpdate_.addKeys.length == 0 && accountUpdate_.removeKeys.length == 0) revert EmptyKeySet();

        uint256 activeValidAfter = _activePendingValidAfter(worldIDAccount_);
        if (activeValidAfter != 0) revert PendingSessionKeyUpdate(activeValidAfter);

        uint256 generation_ = generationOf[worldIDAccount_];
        uint256 signalHash = uint256(keccak256(abi.encode(accountUpdate_, generation_))) >> 8;

        _dispatchSessionProof(
            SessionProofScalars({
                rpId: WORLD_CHAIN_RP_ID,
                proofNonce: proofNonce_,
                signalHash: signalHash,
                expiresAtMin: expiresAtMin_,
                issuerSchemaId: issuerSchemaId_,
                credentialGenesisIssuedAtMin: credentialGenesisIssuedAtMin_,
                sessionId: sessionIdOf[worldIDAccount_]
            }),
            sessionNullifier_,
            proof_
        );

        _revertIfOverlappingKey(accountUpdate_.addKeys, accountUpdate_.removeKeys);
        _snapshotCurrentKeys(worldIDAccount_);

        if (accountUpdate_.removeKeys.length != 0) _removeKeys(worldIDAccount_, accountUpdate_.removeKeys);
        if (accountUpdate_.addKeys.length != 0) _addKeys(worldIDAccount_, accountUpdate_.addKeys);

        uint256 validAfter_ = block.timestamp + sessionKeyUpdateCooldown;
        pendingValidAfter[worldIDAccount_] = validAfter_;
        generationOf[worldIDAccount_] = generation_ + 1;

        emit SessionKeyUpdateScheduled(worldIDAccount_, accountUpdate_.addKeys, accountUpdate_.removeKeys, validAfter_);
    }

    /// @inheritdoc IWorldIDAccountManager
    function revertUpdate(
        address worldIDAccount_,
        uint256 proofNonce_,
        uint64 issuerSchemaId_,
        uint64 expiresAtMin_,
        uint256 credentialGenesisIssuedAtMin_,
        uint256[2] calldata sessionNullifier_,
        uint256[5] calldata proof_
    ) external virtual onlyProxy nonReentrant {
        if (worldIDAccountNullifier[worldIDAccount_] == 0) {
            revert WorldIDAccountDoesNotExist();
        }

        uint256 validAfter_ = pendingValidAfter[worldIDAccount_];
        if (validAfter_ == 0) revert NoPendingSessionKeyUpdate();
        if (block.timestamp >= validAfter_) revert PendingSessionKeyUpdateExpired(validAfter_);

        uint256 generation_ = generationOf[worldIDAccount_];
        uint256 signalHash =
            uint256(keccak256(abi.encode(WORLD_ID_ACCOUNT_REVERT_TAG, worldIDAccount_, validAfter_, generation_))) >> 8;

        _dispatchSessionProof(
            SessionProofScalars({
                rpId: WORLD_CHAIN_RP_ID,
                proofNonce: proofNonce_,
                signalHash: signalHash,
                expiresAtMin: expiresAtMin_,
                issuerSchemaId: issuerSchemaId_,
                credentialGenesisIssuedAtMin: credentialGenesisIssuedAtMin_,
                sessionId: sessionIdOf[worldIDAccount_]
            }),
            sessionNullifier_,
            proof_
        );

        _restorePreviousKeys(worldIDAccount_);
        delete pendingValidAfter[worldIDAccount_];
        generationOf[worldIDAccount_] = generation_ + 1;

        emit SessionKeyUpdateReverted(worldIDAccount_);
    }

    /// @inheritdoc IWorldIDAccountManager
    function getSessionKeyHashes(address worldIDAccount_) external view virtual onlyProxy returns (bytes32[] memory) {
        if (_activePendingValidAfter(worldIDAccount_) != 0) {
            return previousSessionKeyHashes[worldIDAccount_];
        }
        return sessionKeyHashes[worldIDAccount_];
    }

    /// @inheritdoc IWorldIDAccountManager
    function getStoredSessionKeyHashes(address worldIDAccount_)
        external
        view
        virtual
        onlyProxy
        returns (bytes32[] memory)
    {
        return sessionKeyHashes[worldIDAccount_];
    }

    /// @inheritdoc IWorldIDAccountManager
    function getPreviousSessionKeyHashes(address worldIDAccount_)
        external
        view
        virtual
        onlyProxy
        returns (bytes32[] memory)
    {
        if (_activePendingValidAfter(worldIDAccount_) == 0) {
            return new bytes32[](0);
        }
        return previousSessionKeyHashes[worldIDAccount_];
    }

    /// @inheritdoc IWorldIDAccountManager
    function getPendingValidAfter(address worldIDAccount_) external view virtual onlyProxy returns (uint256) {
        return _activePendingValidAfter(worldIDAccount_);
    }

    /// @inheritdoc IWorldIDAccountManager
    function isAuthorized(address worldIDAccount_, bytes calldata key_) external view virtual onlyProxy returns (bool) {
        if (key_.length == 0 || key_.length > MAX_KEY_BYTES) return false;

        bytes32 keyHash = keccak256(key_);
        if (_activePendingValidAfter(worldIDAccount_) != 0) {
            return previousKeyHashIndex[worldIDAccount_][keyHash] != 0;
        }
        return keyHashIndex[worldIDAccount_][keyHash] != 0;
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  ADMIN                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Updates the World ID verifier.
    /// @param worldIDVerifier_ The new verifier. Must not be zero.
    function setWorldIDVerifier(IWorldIDVerifier worldIDVerifier_) external virtual onlyProxy onlyOwner {
        if (address(worldIDVerifier_) == address(0)) revert AddressZero();
        worldIDVerifier = worldIDVerifier_;
        emit WorldIDVerifierSet(address(worldIDVerifier_));
    }

    /// @inheritdoc IWorldIDAccountManager
    function setSessionKeyUpdateCooldown(uint256 sessionKeyUpdateCooldown_) external virtual onlyProxy onlyOwner {
        if (sessionKeyUpdateCooldown_ == 0) revert ZeroCooldown();
        sessionKeyUpdateCooldown = sessionKeyUpdateCooldown_;
        emit SessionKeyUpdateCooldownSet(sessionKeyUpdateCooldown_);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                INTERNAL                                 ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @dev Returns `pendingValidAfter[worldIDAccount_]` while the pending update is active.
    function _activePendingValidAfter(address worldIDAccount_) internal view returns (uint256) {
        uint256 validAfter_ = pendingValidAfter[worldIDAccount_];
        if (validAfter_ == 0 || block.timestamp >= validAfter_) return 0;
        return validAfter_;
    }

    /// @dev Dispatches the session-proof verifier call. Inputs are bundled into a memory struct
    ///      to keep call sites small enough to compile without `via_ir`.
    function _dispatchSessionProof(
        SessionProofScalars memory s_,
        uint256[2] calldata sessionNullifier_,
        uint256[5] calldata proof_
    ) internal view {
        worldIDVerifier.verifySession(
            s_.rpId,
            s_.proofNonce,
            s_.signalHash,
            s_.expiresAtMin,
            s_.issuerSchemaId,
            s_.credentialGenesisIssuedAtMin,
            s_.sessionId,
            sessionNullifier_,
            proof_
        );
    }

    /// @dev Rejects a unified update that mentions the same key in both `addKeys_` and
    ///      `removeKeys_`. Overlap is treated as a malformed payload rather than a billed no-op.
    function _revertIfOverlappingKey(bytes[] calldata addKeys_, bytes[] calldata removeKeys_) internal pure {
        for (uint256 i = 0; i < addKeys_.length; ++i) {
            bytes32 addHash = keccak256(addKeys_[i]);
            for (uint256 j = 0; j < removeKeys_.length; ++j) {
                if (addHash == keccak256(removeKeys_[j])) {
                    revert OverlappingUpdateKey(addHash);
                }
            }
        }
    }

    /// @dev Validates and appends each key in `keys_` to the stored set for `worldIDAccount_`.
    function _addKeys(address worldIDAccount_, bytes[] calldata keys_) internal {
        uint256 keysLen = keys_.length;
        if (keysLen == 0) revert EmptyKeySet();

        bytes32[] storage hashArr = sessionKeyHashes[worldIDAccount_];
        mapping(bytes32 => uint256) storage idxMap = keyHashIndex[worldIDAccount_];

        uint256 newTotal = hashArr.length + keysLen;
        if (newTotal > MAX_SESSION_KEYS) revert TooManyKeys(newTotal, MAX_SESSION_KEYS);

        for (uint256 i = 0; i < keysLen; ++i) {
            bytes calldata key = keys_[i];
            _validateKey(key);
            bytes32 hash = keccak256(key);
            if (idxMap[hash] != 0) revert DuplicateSessionKey(hash);

            hashArr.push(hash);
            idxMap[hash] = hashArr.length;
        }
    }

    /// @dev Removes each key in `keys_` from the stored set via swap-and-pop.
    function _removeKeys(address worldIDAccount_, bytes[] calldata keys_) internal {
        uint256 keysLen = keys_.length;
        if (keysLen == 0) revert EmptyKeySet();

        bytes32[] storage hashArr = sessionKeyHashes[worldIDAccount_];
        mapping(bytes32 => uint256) storage idxMap = keyHashIndex[worldIDAccount_];

        for (uint256 i = 0; i < keysLen; ++i) {
            bytes calldata key = keys_[i];
            _validateKey(key);
            bytes32 hash = keccak256(key);
            uint256 idxPlusOne = idxMap[hash];
            if (idxPlusOne == 0) revert UnknownSessionKey(hash);

            uint256 idx = idxPlusOne - 1;
            uint256 lastIdx = hashArr.length - 1;
            if (idx != lastIdx) {
                bytes32 lastHash = hashArr[lastIdx];
                hashArr[idx] = lastHash;
                idxMap[lastHash] = idxPlusOne;
            }
            hashArr.pop();
            delete idxMap[hash];
        }
    }

    /// @dev Copies the currently stored key set into the previous-key snapshot, replacing any
    ///      stale snapshot from an expired or reverted update window.
    function _snapshotCurrentKeys(address worldIDAccount_) internal {
        _clearPreviousKeys(worldIDAccount_);

        bytes32[] storage current = sessionKeyHashes[worldIDAccount_];
        bytes32[] storage previous = previousSessionKeyHashes[worldIDAccount_];
        mapping(bytes32 => uint256) storage previousIdx = previousKeyHashIndex[worldIDAccount_];

        for (uint256 i = 0; i < current.length; ++i) {
            bytes32 hash = current[i];
            previous.push(hash);
            previousIdx[hash] = previous.length;
        }
    }

    /// @dev Restores the previous-key snapshot into primary storage, then clears the snapshot.
    function _restorePreviousKeys(address worldIDAccount_) internal {
        _clearCurrentKeys(worldIDAccount_);

        bytes32[] storage previous = previousSessionKeyHashes[worldIDAccount_];
        bytes32[] storage current = sessionKeyHashes[worldIDAccount_];
        mapping(bytes32 => uint256) storage currentIdx = keyHashIndex[worldIDAccount_];

        for (uint256 i = 0; i < previous.length; ++i) {
            bytes32 hash = previous[i];
            current.push(hash);
            currentIdx[hash] = current.length;
        }

        _clearPreviousKeys(worldIDAccount_);
    }

    /// @dev Clears the primary stored key set and its index map.
    function _clearCurrentKeys(address worldIDAccount_) internal {
        bytes32[] storage current = sessionKeyHashes[worldIDAccount_];
        mapping(bytes32 => uint256) storage currentIdx = keyHashIndex[worldIDAccount_];

        for (uint256 i = 0; i < current.length; ++i) {
            delete currentIdx[current[i]];
        }
        delete sessionKeyHashes[worldIDAccount_];
    }

    /// @dev Clears the previous-key snapshot and its index map.
    function _clearPreviousKeys(address worldIDAccount_) internal {
        bytes32[] storage previous = previousSessionKeyHashes[worldIDAccount_];
        mapping(bytes32 => uint256) storage previousIdx = previousKeyHashIndex[worldIDAccount_];

        for (uint256 i = 0; i < previous.length; ++i) {
            delete previousIdx[previous[i]];
        }
        delete previousSessionKeyHashes[worldIDAccount_];
    }

    /// @dev Reverts if `key_` violates the per-key length bounds.
    function _validateKey(bytes calldata key_) internal pure {
        if (key_.length == 0) revert EmptyKey();
        if (key_.length > MAX_KEY_BYTES) revert KeyTooLarge(key_.length, MAX_KEY_BYTES);
    }
}
