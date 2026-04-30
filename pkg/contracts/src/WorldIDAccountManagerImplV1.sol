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

    /// @notice Maximum authorized authenticator keys per World ID Account.
    uint256 public constant MAX_SESSION_KEYS = 20;

    /// @notice Maximum byte length of a single authenticator key.
    uint256 public constant MAX_KEY_BYTES = 128;

    ///////////////////////////////////////////////////////////////////////////////
    ///                             STATE VARIABLES                             ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice The World ID verifier proxy address.
    IWorldIDVerifier public worldIDVerifier;

    /// @notice A map from World ID accounts to their single use creation nullifier.
    mapping(address worldIDAccount => uint256 nullifier) public worldIDAccountNullifier;

    /// @notice The session identifier installed at creation, used to scope subsequent Session Proofs.
    mapping(address worldIDAccount => uint256 sessionId) public sessionIdOf;

    /// @notice Per-account monotonic counter folded into update signal hashes so identical
    ///         payloads still yield distinct signals across updates.
    mapping(address worldIDAccount => uint256 generation) public generationOf;

    /// @notice Hashes of the currently authorized authenticator keys for each account.
    mapping(address worldIDAccount => bytes32[] keyHashes) public sessionKeyHashes;

    /// @notice 1-indexed position in `sessionKeyHashes[acct]` of each key hash. `0` means absent.
    mapping(address worldIDAccount => mapping(bytes32 keyHash => uint256 indexPlusOne)) public keyHashIndex;

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
    function initialize(IWorldIDVerifier worldIDVerifier_, address owner_) external reinitializer(1) {
        if (address(worldIDVerifier_) == address(0)) revert AddressZero();
        if (owner_ == address(0)) revert AddressZero();

        __Base_init(owner_);
        worldIDVerifier = worldIDVerifier_;

        emit WorldIDAccountManagerImplInitialized(worldIDVerifier_, owner_);
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
        if (createUpdate_.operation != Operation.Create) {
            revert InvalidOperation();
        }
        // A zero nullifier would be indistinguishable from "absent" in `worldIDAccountNullifier`,
        // letting the same account be re-created and permanently bricking `update`.
        if (worldIDAccountNullifier_ == 0) {
            revert ZeroNullifier();
        }

        worldIDAccount_ = address(uint160(uint256(keccak256(abi.encodePacked(worldIDAccountNullifier_)))));
        if (worldIDAccountNullifier[worldIDAccount_] != 0) {
            revert WorldIDAccountAlreadyExists();
        }

        uint256 action = uint256(keccak256(abi.encodePacked(WORLD_ID_ACCOUNT_TAG, worldIDAccountNonce_))) >> 8;
        uint256 signalHash = uint256(keccak256(abi.encode(createUpdate_))) >> 8;

        worldIDAccountNullifier[worldIDAccount_] = worldIDAccountNullifier_;
        sessionIdOf[worldIDAccount_] = sessionId_;

        _addKeys(worldIDAccount_, createUpdate_.keys);

        emit WorldIDAccountCreated(worldIDAccount_, worldIDAccountNullifier_, sessionId_);
        emit SessionKeysAdded(worldIDAccount_, createUpdate_.keys);

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
    }

    /// @inheritdoc IWorldIDAccountManager
    /// @dev Follows checks-effects-interactions: the signal hash is snapshotted from the current
    ///      generation before the generation is bumped and key changes are applied; the verifier
    ///      call happens last with that snapshotted hash. Revert in the verifier rolls back all
    ///      preceding writes via the EVM.
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
        // CHECKS
        if (accountUpdate_.operation == Operation.Create) {
            revert InvalidOperation();
        }
        if (worldIDAccountNullifier[worldIDAccount_] == 0) {
            revert WorldIDAccountDoesNotExist();
        }

        // Snapshot signal hash with the pre-bump generation so the verifier still validates
        // the proof against the generation the caller signed over.
        uint256 signalHash = uint256(keccak256(abi.encode(accountUpdate_, generationOf[worldIDAccount_]))) >> 8;

        // EFFECTS
        ++generationOf[worldIDAccount_];

        if (accountUpdate_.operation == Operation.Add) {
            _addKeys(worldIDAccount_, accountUpdate_.keys);
            emit SessionKeysAdded(worldIDAccount_, accountUpdate_.keys);
        } else {
            _removeKeys(worldIDAccount_, accountUpdate_.keys);
            emit SessionKeysRemoved(worldIDAccount_, accountUpdate_.keys);
        }

        // INTERACTION
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
    }

    /// @inheritdoc IWorldIDAccountManager
    function getSessionKeyHashes(address worldIDAccount_) external view virtual onlyProxy returns (bytes32[] memory) {
        return sessionKeyHashes[worldIDAccount_];
    }

    /// @inheritdoc IWorldIDAccountManager
    function isAuthorized(address worldIDAccount_, bytes calldata key_) external view virtual onlyProxy returns (bool) {
        if (key_.length == 0 || key_.length > MAX_KEY_BYTES) return false;
        return keyHashIndex[worldIDAccount_][keccak256(key_)] != 0;
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

    ///////////////////////////////////////////////////////////////////////////////
    ///                                INTERNAL                                 ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @dev Dispatches the session-proof verifier call. Inputs are bundled into a memory struct
    ///      to keep `update`'s frame small enough to compile without `via_ir`.
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

    /// @dev Validates and appends each key in `keys_` to the authorized set for `worldIDAccount_`.
    ///      Reverts on length violations, payload duplicates, or set-size overflow.
    function _addKeys(address worldIDAccount_, bytes[] calldata keys_) internal {
        uint256 keysLen = keys_.length;
        if (keysLen == 0) revert EmptyKeySet();

        bytes32[] storage hashArr = sessionKeyHashes[worldIDAccount_];
        mapping(bytes32 => uint256) storage idxMap = keyHashIndex[worldIDAccount_];

        uint256 newTotal = hashArr.length + keysLen;
        if (newTotal > MAX_SESSION_KEYS) {
            revert TooManyKeys(newTotal, MAX_SESSION_KEYS);
        }

        for (uint256 i = 0; i < keysLen; ++i) {
            bytes calldata key = keys_[i];
            _validateKey(key);
            bytes32 hash = keccak256(key);
            if (idxMap[hash] != 0) revert DuplicateSessionKey(hash);

            hashArr.push(hash);
            idxMap[hash] = hashArr.length;
        }
    }

    /// @dev Removes each key in `keys_` from the authorized set via swap-and-pop.
    ///      Reverts on length violations, in-payload duplicates, or unknown keys.
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

    /// @dev Reverts if `key_` violates the per-key length bounds.
    function _validateKey(bytes calldata key_) internal pure {
        if (key_.length == 0) revert EmptyKey();
        if (key_.length > MAX_KEY_BYTES) {
            revert KeyTooLarge(key_.length, MAX_KEY_BYTES);
        }
    }
}
