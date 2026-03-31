// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IWorldIDAccountFactory} from "./interfaces/IWorldIDAccountFactory.sol";
import {IWorldIDVerifier} from "./interfaces/IWorldIDVerifier.sol";
import {WorldIDAccountStorage} from "./libraries/WorldIDAccountStorage.sol";

/// @title World ID Account Factory
/// @author Worldcoin
/// @notice Core registry/factory for World ID Accounts.
///         Verifies two ZK proofs (creation + account), derives a deterministic account address
///         from the account nullifier, and stores authorized signing keys.
/// @dev In production this will be a precompile at address 0x1D that writes directly to account
///      storage. This Solidity reference implementation stores everything in the factory's own
///      mappings instead.
///
/// TODO(precompile): In production, write directly to account storage via precompile host API.
///
/// TODO: The monotonic generation counter stored on-chain enables linking successive account
/// generations for the same identity via the shared creationNullifier. A future upgrade should
/// explore chained nullifier schemes for cross-generation unlinkability.
///
/// TODO: 0x6f transactions with a non-empty authorization_list must be rejected (prevents
/// re-delegation away from WorldIDAccountDelegate). Enforce this at the protocol/pool level.
///
/// @custom:security-contact security@toolsforhumanity.com
contract WorldIDAccountFactory is IWorldIDAccountFactory {
    ///////////////////////////////////////////////////////////////////////////////
    ///                              CONSTANTS                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Transaction type identifier for World ID Account transactions.
    uint8 public constant WORLD_TX_TYPE = 0x6f;

    /// @notice The relying-party identifier for World Chain.
    uint64 public constant WORLD_CHAIN_RP_ID = 480;

    /// @notice Maximum number of authorized keys per account.
    uint256 public constant MAX_AUTHORIZED_KEYS = 20;

    /// @notice Reserved precompile address for production deployment.
    address public constant WORLD_ID_ACCOUNT_FACTORY = address(0x1D);

    /// @notice Expected key data length for compressed secp256k1 public keys.
    uint256 private constant SECP256K1_KEY_LENGTH = 33;

    /// @notice Expected key data length for P256 / WebAuthn public keys (x ‖ y).
    uint256 private constant P256_KEY_LENGTH = 64;

    /// @dev Pre-computed action hash for creation proofs.
    uint256 private immutable CREATION_ACTION;

    ///////////////////////////////////////////////////////////////////////////////
    ///                             STATE VARIABLES                             ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice The World ID v4 OPRF verifier contract.
    IWorldIDVerifier public worldIDVerifier;

    /// @notice Maps creationNullifier → generation count.
    /// @dev Generation 0 = initial account creation, generation N > 0 = account recycling/key rotation.
    mapping(uint256 => uint256) public generations;

    /// @notice Tracks whether an account nullifier has already been consumed.
    mapping(uint256 => bool) public spentAccountNullifiers;

    /// @notice Tracks whether an account address has been destroyed.
    mapping(address => bool) public destroyed;

    // ---- Solidity-only storage (not used in the precompile version) ----------

    /// @dev Maps account address → authorized keys.
    /// TODO(precompile): In production, keys are written directly to account storage.
    mapping(address => AuthorizedKey[]) internal _accountKeys;

    /// @dev Maps account address → generation at creation time.
    mapping(address => uint256) internal _accountGeneration;

    /// @dev Maps account address → account nullifier.
    mapping(address => uint256) internal _accountNullifiers;

    ///////////////////////////////////////////////////////////////////////////////
    ///                               CONSTRUCTOR                               ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Deploys the factory with a reference to the World ID verifier.
    /// @param _worldIDVerifier The address of the deployed IWorldIDVerifier contract.
    constructor(IWorldIDVerifier _worldIDVerifier) {
        worldIDVerifier = _worldIDVerifier;
        CREATION_ACTION = uint256(keccak256("WORLD_ID_ACCOUNT_CREATION"));
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                            EXTERNAL FUNCTIONS                            ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @inheritdoc IWorldIDAccountFactory
    function createAccount(
        uint256 creationNullifier,
        uint256 accountNullifier,
        uint256[5] calldata creationProof,
        uint256[5] calldata accountProof,
        uint64 expiresAtMin,
        uint64 issuerSchemaId,
        AuthorizedKey[] calldata keys
    ) external returns (address account) {
        // 1. Read current generation
        uint256 g = generations[creationNullifier];

        // 2. Verify creation proof
        _verifyCreationProof(creationNullifier, expiresAtMin, issuerSchemaId, creationProof);

        // 3. Verify account proof
        _verifyAccountProof(accountNullifier, g, keys, expiresAtMin, issuerSchemaId, accountProof);

        // 4. Check nullifier uniqueness
        if (spentAccountNullifiers[accountNullifier]) {
            revert NullifierAlreadySpent();
        }

        // 5. Mark nullifier as spent
        spentAccountNullifiers[accountNullifier] = true;

        // 6. Derive deterministic account address
        account = address(bytes20(keccak256(abi.encodePacked(accountNullifier))));

        // 7. Ensure account has not been destroyed
        if (destroyed[account]) {
            revert AccountAlreadyDestroyed();
        }

        // 8. Validate key count
        if (keys.length == 0 || keys.length > MAX_AUTHORIZED_KEYS) {
            revert InvalidKeyCount();
        }

        // 9. Validate each key's data length
        for (uint256 i = 0; i < keys.length; ++i) {
            _validateKeyData(keys[i]);
        }

        // 10. Store keys and metadata
        // TODO(precompile): In production, write directly to account storage via precompile host API.
        //      The precompile writes to the slots defined in WorldIDAccountStorage.
        _storeAccountData(account, g, accountNullifier, keys);

        // 11. Advance generation
        generations[creationNullifier] = g + 1;

        // 12. Emit event
        emit AccountCreated(account, g);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              VIEW FUNCTIONS                              ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Returns the authorized keys for an account.
    /// @param account The account address.
    /// @return The array of authorized keys.
    function getAccountKeys(address account) external view returns (AuthorizedKey[] memory) {
        return _accountKeys[account];
    }

    /// @notice Returns the generation stored for an account.
    /// @param account The account address.
    /// @return The generation number.
    function getAccountGeneration(address account) external view returns (uint256) {
        return _accountGeneration[account];
    }

    /// @notice Returns the account nullifier stored for an account.
    /// @param account The account address.
    /// @return The account nullifier.
    function getAccountNullifier(address account) external view returns (uint256) {
        return _accountNullifiers[account];
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                            INTERNAL FUNCTIONS                            ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @dev Verifies the creation ZK proof.
    /// @param creationNullifier The nullifier for the creation proof.
    /// @param expiresAtMin Minimum expiration timestamp (minutes).
    /// @param issuerSchemaId The credential issuer schema identifier.
    /// @param proof The five-element ZK proof array.
    function _verifyCreationProof(
        uint256 creationNullifier,
        uint64 expiresAtMin,
        uint64 issuerSchemaId,
        uint256[5] calldata proof
    ) internal view {
        // action  = keccak256("WORLD_ID_ACCOUNT_CREATION")
        // rpId    = 480 (World Chain)
        // nonce   = 0
        // signalHash = 0
        worldIDVerifier.verify(
            creationNullifier,
            CREATION_ACTION,
            WORLD_CHAIN_RP_ID,
            0, // nonce
            0, // signalHash
            expiresAtMin,
            issuerSchemaId,
            0, // credentialGenesisIssuedAtMin
            proof
        );
    }

    /// @dev Verifies the account ZK proof binding keys to the account generation.
    /// @param accountNullifier The nullifier for the account proof.
    /// @param g The current generation counter.
    /// @param keys The authorized keys being bound.
    /// @param expiresAtMin Minimum expiration timestamp (minutes).
    /// @param issuerSchemaId The credential issuer schema identifier.
    /// @param proof The five-element ZK proof array.
    function _verifyAccountProof(
        uint256 accountNullifier,
        uint256 g,
        AuthorizedKey[] calldata keys,
        uint64 expiresAtMin,
        uint64 issuerSchemaId,
        uint256[5] calldata proof
    ) internal view {
        // action     = keccak256(abi.encodePacked(rpId, uint256(0), g))
        // signalHash = keccak256(abi.encode(keys)) >> 8
        uint256 accountAction = uint256(keccak256(abi.encodePacked(WORLD_CHAIN_RP_ID, uint256(0), g)));
        uint256 signalHash = uint256(keccak256(abi.encode(keys))) >> 8;

        worldIDVerifier.verify(
            accountNullifier,
            accountAction,
            WORLD_CHAIN_RP_ID,
            0, // nonce
            signalHash,
            expiresAtMin,
            issuerSchemaId,
            0, // credentialGenesisIssuedAtMin
            proof
        );
    }

    /// @dev Validates that key data has the correct length for its auth type.
    /// @param key The authorized key to validate.
    function _validateKeyData(AuthorizedKey calldata key) internal pure {
        if (key.authType == AuthType.Secp256k1) {
            if (key.keyData.length != SECP256K1_KEY_LENGTH) {
                revert InvalidKeyData();
            }
        } else if (key.authType == AuthType.P256 || key.authType == AuthType.WebAuthn) {
            if (key.keyData.length != P256_KEY_LENGTH) {
                revert InvalidKeyData();
            }
        }
    }

    /// @dev Stores account data in factory-local storage.
    /// @param account The derived account address.
    /// @param g The generation counter.
    /// @param accountNullifier The account nullifier.
    /// @param keys The authorized keys to store.
    function _storeAccountData(address account, uint256 g, uint256 accountNullifier, AuthorizedKey[] calldata keys)
        internal
    {
        delete _accountKeys[account];
        for (uint256 i = 0; i < keys.length; ++i) {
            _accountKeys[account].push(keys[i]);
        }
        _accountGeneration[account] = g;
        _accountNullifiers[account] = accountNullifier;
    }
}
