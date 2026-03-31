// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @title IWorldIDAccountFactory
/// @author Worldcoin
/// @notice Interface for the World ID Account factory/registry contract.
/// @dev In production this will be a precompile at address 0x1D.
/// @custom:security-contact security@toolsforhumanity.com
interface IWorldIDAccountFactory {
    /// @notice The type of cryptographic key used for transaction authorization.
    enum AuthType {
        Secp256k1,
        P256,
        WebAuthn
    }

    /// @notice An authorized signing key for a World ID Account.
    /// @param authType The cryptographic scheme for this key.
    /// @param keyData The raw public key bytes (33 bytes for Secp256k1, 64 bytes for P256/WebAuthn).
    struct AuthorizedKey {
        AuthType authType;
        bytes keyData;
    }

    /// @notice Emitted when a new World ID Account is created.
    /// @param account The derived account address.
    /// @param generation The generation counter at creation time.
    event AccountCreated(address indexed account, uint256 generation);

    /// @notice Emitted when a World ID Account is destroyed.
    /// @param account The destroyed account address.
    /// @param generation The generation counter at destruction time.
    event AccountDestroyed(address indexed account, uint256 generation);

    /// @notice Thrown when the account nullifier has already been used.
    error NullifierAlreadySpent();

    /// @notice Thrown when the account has been destroyed and cannot be reused.
    error AccountAlreadyDestroyed();

    /// @notice Thrown when the number of authorized keys is zero or exceeds the maximum.
    error InvalidKeyCount();

    /// @notice Thrown when key data has an incorrect length for its auth type.
    error InvalidKeyData();

    /// @notice Creates a new World ID Account (or recycles via a new generation).
    /// @param creationNullifier The nullifier binding the creation proof to this identity.
    /// @param accountNullifier The nullifier binding the account proof to this generation.
    /// @param creationProof The ZK proof for account creation authorization.
    /// @param accountProof The ZK proof binding keys to this account generation.
    /// @param expiresAtMin The minimum expiration timestamp (in minutes) for proof validity.
    /// @param issuerSchemaId The credential issuer schema identifier.
    /// @param keys The initial set of authorized signing keys.
    /// @return account The derived account address.
    function createAccount(
        uint256 creationNullifier,
        uint256 accountNullifier,
        uint256[5] calldata creationProof,
        uint256[5] calldata accountProof,
        uint64 expiresAtMin,
        uint64 issuerSchemaId,
        AuthorizedKey[] calldata keys
    ) external returns (address account);

    /// @notice Returns the current generation for a given creation nullifier.
    /// @param creationNullifier The creation nullifier to query.
    /// @return The current generation count.
    function generations(uint256 creationNullifier) external view returns (uint256);

    /// @notice Returns whether an account nullifier has been spent.
    /// @param accountNullifier The account nullifier to query.
    /// @return True if the nullifier has been spent.
    function spentAccountNullifiers(uint256 accountNullifier) external view returns (bool);

    /// @notice Returns whether an account address has been destroyed.
    /// @param account The account address to query.
    /// @return True if the account has been destroyed.
    function destroyed(address account) external view returns (bool);
}
