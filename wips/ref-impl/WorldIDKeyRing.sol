// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IWorldIDKeyRing} from "./IWorldIDKeyRing.sol";

interface IWorldIDVerifier {
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

    function verifyProofAndSignals(
        uint256 nullifier,
        uint256 action,
        uint64 rpId,
        uint256 nonce,
        uint256 signalHash,
        uint64 expiresAtMin,
        uint64 issuerSchemaId,
        uint256 credentialGenesisIssuedAtMin,
        uint256 sessionId,
        uint256[5] calldata zeroKnowledgeProof
    ) external view;
}

/// @title WorldIDKeyRing — WIP-1001 reference implementation
/// @notice Reference Solidity implementation. 
contract WorldIDKeyRing is IWorldIDKeyRing {
    ////////////////////////////////////////////////////////////
    //                         CONSTANTS                      //
    ////////////////////////////////////////////////////////////

    /// @notice Address occupied by the precompile in the production EVM.
    /// @dev Solidity `address(0x1D)` matches the WIP-1001 constant
    ///      `0x000000000000000000000000000000000000001D`.
    address public constant WORLD_ID_KEYRING_PRECOMPILE =
        address(uint160(0x1D));

    /// @notice World Chain registered RP identifier.
    uint64 public constant WORLD_CHAIN_RP_ID = 480;

    /// @notice Domain tag prefixing the creation action (UTF-8, 16 bytes).
    bytes16 public constant WORLD_ID_KEYRING_TAG = bytes16("WORLD_ID_KEYRING");

    /// @notice Maximum number of session keys authorized per keyring.
    uint256 public constant MAX_SESSION_KEYS = 20;

    // Required `keyData` lengths per `KeyType`.
    uint256 private constant SECP256K1_KEY_LEN = 33;
    uint256 private constant P256_KEY_LEN = 64;
    uint256 private constant WEBAUTHN_KEY_LEN = 64;
    uint256 private constant ED25519_KEY_LEN = 32;

    ////////////////////////////////////////////////////////////
    //                          STORAGE                       //
    ////////////////////////////////////////////////////////////

    /// @notice The World ID 4.0 verifier used to gate creation and updates.
    IWorldIDVerifier public immutable WORLD_ID_VERIFIER;

    /// @dev Per-keyring creation nullifier `ν_k`. Non-zero iff the keyring exists.
    mapping(address keyring => uint256) private _keyringNullifier;

    /// @dev Per-keyring stored `sessionId` (set at creation, retained across updates).
    mapping(address keyring => uint256) private _sessionId;

    /// @dev Per-keyring monotonic update counter folded into Session Proof signals
    ///      to ensure each update produces a unique `signalHash` even when the
    ///      `KeyringUpdate` payload is byte-equal to a prior one.
    mapping(address keyring => uint256) private _generation;

    /// @dev Authorized session keys, addressable by index for enumeration.
    mapping(address keyring => SessionKey[]) private _sessionKeys;

    /// @dev Reverse index: `keccak256(packedKey) => array index + 1`. Zero means
    ///      the key is not present (so we can use length-0 default semantics).
    mapping(address keyring => mapping(bytes32 keyId => uint256 indexPlusOne))
        private _keyIndex;

    ////////////////////////////////////////////////////////////
    //                       CONSTRUCTION                     //
    ////////////////////////////////////////////////////////////

    constructor(IWorldIDVerifier worldIdVerifier) {
        WORLD_ID_VERIFIER = worldIdVerifier;
    }

    ////////////////////////////////////////////////////////////
    //                       MUTATIVE API                     //
    ////////////////////////////////////////////////////////////

    /// @inheritdoc IWorldIDKeyRing
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
    ) external returns (address keyringAddress) {
        // (1) Mode must be Create.
        if (createUpdate.mode != UpdateMode.Create) revert InvalidUpdateMode();

        // (2) Recompute the creation action.
        uint256 action = uint256(
            keccak256(abi.encodePacked(WORLD_ID_KEYRING_TAG, keyringNonce))
        );

        // (3) Recompute the signal hash; >> 8 fits the BN254 scalar field.
        uint256 signalHash = uint256(keccak256(abi.encode(createUpdate))) >> 8;

        // (4) Verify the World ID 4.0 Uniqueness Proof. The verifier reverts on
        //     failure; on success the supplied `keyringNullifier` is bound to
        //     `(rpId, action)` by the circuit's nullifier derivation.
        _verifyUniquenessProof(
            keyringNullifier,
            action,
            proofNonce,
            signalHash,
            expiresAtMin,
            issuerSchemaId,
            credentialGenesisIssuedAtMin,
            proof
        );

        // (5) Derive the lifetime-stable keyring address.
        keyringAddress = address(
            bytes20(keccak256(abi.encode(keyringNullifier)))
        );

        if (_keyringNullifier[keyringAddress] != 0)
            revert KeyringAlreadyExists();

        _keyringNullifier[keyringAddress] = keyringNullifier;
        _sessionId[keyringAddress] = sessionId;

        _applyAdds(keyringAddress, createUpdate.keys);

        emit KeyringCreated(keyringAddress, keyringNullifier, sessionId);
        emit SessionKeysAdded(keyringAddress, createUpdate.keys);
    }

    /// @inheritdoc IWorldIDKeyRing
    function update(
        address keyring,
        uint256 proofNonce,
        uint64 issuerSchemaId,
        uint64 expiresAtMin,
        uint256 credentialGenesisIssuedAtMin,
        uint256[2] calldata sessionNullifier,
        uint256[5] calldata proof,
        KeyringUpdate calldata keyringUpdate
    ) external {
        if (keyringUpdate.mode == UpdateMode.Create) revert InvalidUpdateMode();
        if (_keyringNullifier[keyring] == 0) revert KeyringDoesNotExist();

        uint256 sessionId = _sessionId[keyring];
        // Bind the proof to a per-keyring generational counter so duplicate
        // payloads yield distinct signals (see WIP-1001 §Proof Authorization).
        uint256 generation = _generation[keyring];
        uint256 signalHash = uint256(
            keccak256(abi.encode(keyringUpdate, generation))
        ) >> 8;

        _verifySessionProof(
            proofNonce,
            signalHash,
            expiresAtMin,
            issuerSchemaId,
            credentialGenesisIssuedAtMin,
            sessionId,
            sessionNullifier,
            proof
        );

        unchecked {
            // Generation only increments on a verified update; overflow is
            // effectively impossible under any realistic workload.
            _generation[keyring] = generation + 1;
        }

        if (keyringUpdate.mode == UpdateMode.Add) {
            _applyAdds(keyring, keyringUpdate.keys);
            emit SessionKeysAdded(keyring, keyringUpdate.keys);
        } else {
            // UpdateMode.Remove
            _applyRemoves(keyring, keyringUpdate.keys);
            emit SessionKeysRemoved(keyring, keyringUpdate.keys);
        }
    }

    ////////////////////////////////////////////////////////////
    //                         VIEW API                       //
    ////////////////////////////////////////////////////////////

    /// @inheritdoc IWorldIDKeyRing
    function getSessionKeys(
        address keyring
    ) external view returns (SessionKey[] memory) {
        return _sessionKeys[keyring];
    }

    /// @inheritdoc IWorldIDKeyRing
    function isAuthorized(
        address keyring,
        SessionKey calldata key
    ) external view returns (bool) {
        bytes memory data = key.keyData;
        return _keyIndex[keyring][_keyId(key.keyType, data)] != 0;
    }

    /// @inheritdoc IWorldIDKeyRing
    function getKeyringNullifier(
        address keyring
    ) external view returns (uint256) {
        return _keyringNullifier[keyring];
    }

    ////////////////////////////////////////////////////////////
    //                  VERIFIER HOOKS (virtual)              //
    ////////////////////////////////////////////////////////////

    /// @dev Indirection so test harnesses can stub proof verification without
    ///      reaching for `vm.mockCall`.
    function _verifyUniquenessProof(
        uint256 keyringNullifier,
        uint256 action,
        uint256 proofNonce,
        uint256 signalHash,
        uint64 expiresAtMin,
        uint64 issuerSchemaId,
        uint256 credentialGenesisIssuedAtMin,
        uint256[5] calldata proof
    ) internal view virtual {
        WORLD_ID_VERIFIER.verify(
            keyringNullifier,
            action,
            WORLD_CHAIN_RP_ID,
            proofNonce,
            signalHash,
            expiresAtMin,
            issuerSchemaId,
            credentialGenesisIssuedAtMin,
            proof
        );
    }

    /// @dev Indirection so test harnesses can stub proof verification without
    ///      reaching for `vm.mockCall`.
    function _verifySessionProof(
        uint256 proofNonce,
        uint256 signalHash,
        uint64 expiresAtMin,
        uint64 issuerSchemaId,
        uint256 credentialGenesisIssuedAtMin,
        uint256 sessionId,
        uint256[2] calldata sessionNullifier,
        uint256[5] calldata proof
    ) internal view virtual {
        WORLD_ID_VERIFIER.verifySession(
            WORLD_CHAIN_RP_ID,
            proofNonce,
            signalHash,
            expiresAtMin,
            issuerSchemaId,
            credentialGenesisIssuedAtMin,
            sessionId,
            sessionNullifier,
            proof
        );
    }

    ////////////////////////////////////////////////////////////
    //                       INTERNAL HELPERS                 //
    ////////////////////////////////////////////////////////////

    function _applyAdds(address keyring, SessionKey[] calldata keys) internal {
        uint256 n = keys.length;
        if (n == 0) revert EmptyKeySet();

        SessionKey[] storage set = _sessionKeys[keyring];
        uint256 currentLen = set.length;
        if (currentLen + n > MAX_SESSION_KEYS) revert TooManyKeys();

        for (uint256 i = 0; i < n; ++i) {
            SessionKey calldata key = keys[i];
            _validateKeyData(key.keyType, key.keyData.length);

            bytes32 id = _keyId(key.keyType, key.keyData);
            if (_keyIndex[keyring][id] != 0) revert DuplicateSessionKey();

            set.push(key);
            // Store index + 1 so a missing key reads as 0.
            _keyIndex[keyring][id] = set.length;
        }
    }

    function _applyRemoves(
        address keyring,
        SessionKey[] calldata keys
    ) internal {
        uint256 n = keys.length;
        if (n == 0) revert EmptyKeySet();

        SessionKey[] storage set = _sessionKeys[keyring];

        for (uint256 i = 0; i < n; ++i) {
            SessionKey calldata key = keys[i];
            bytes32 id = _keyId(key.keyType, key.keyData);

            uint256 indexPlusOne = _keyIndex[keyring][id];
            if (indexPlusOne == 0) revert UnknownSessionKey();

            uint256 idx = indexPlusOne - 1;
            uint256 lastIdx = set.length - 1;

            // Swap-and-pop, fixing up the moved element's index.
            if (idx != lastIdx) {
                SessionKey storage moved = set[lastIdx];
                set[idx] = moved;
                _keyIndex[keyring][_keyId(moved.keyType, moved.keyData)] =
                    idx +
                    1;
            }
            set.pop();
            delete _keyIndex[keyring][id];
        }
        // Per WIP-1001: an empty set is permitted; the keyring becomes
        // unable to sign until the next `Add`.
    }

    function _validateKeyData(
        KeyType keyType,
        uint256 actualLen
    ) internal pure {
        uint256 expected;
        if (keyType == KeyType.Secp256k1) {
            expected = SECP256K1_KEY_LEN;
        } else if (keyType == KeyType.P256) {
            expected = P256_KEY_LEN;
        } else if (keyType == KeyType.WebAuthn) {
            expected = WEBAUTHN_KEY_LEN;
        } else {
            // KeyType.EdDSA
            expected = ED25519_KEY_LEN;
        }
        if (actualLen != expected)
            revert InvalidKeyDataLength(uint8(keyType), actualLen);
    }

    function _keyId(
        KeyType keyType,
        bytes memory keyData
    ) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(uint8(keyType), keyData));
    }
}
