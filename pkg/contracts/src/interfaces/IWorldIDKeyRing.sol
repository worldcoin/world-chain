// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @title WorldChainKeyRing
/// @author Worldcoin Contributors
/// @notice A contract which manages the creation of and management of session keys for World ID KeyChains.
interface IWorldIDKeyRing {
    /// @notice A struct representing an update to the Key Ring.
    struct KeyRingUpdate {
        /// @param updateType The type flag for the Key Ring update.
        /// Flags:
        ///     0x0 = Create 
        ///     0x1 = Add Key
        ///     0x2 = Remove Key
        uint8 updateType;
    }

    struct SessionKey {
        /// TODO: FIXME:
        uint256 key;
    }
    
    /// @notice Creates a new Key Ring following the specification of WIP-1001.
    /// @param keyringCreationNullifier The creation nullifier produced from the OPRF proof of the Key Ring creation.
    /// @param keyringNonce The nonce produced from the OPRF proof of the Key Ring creation.
    /// @param proofNonce The nonce produced from the ZK proof of the Key Ring creation.
    /// @param sessionId The session ID produced from the ZK proof of the Key Ring creation.
    /// @param issuerSchemaId The issuer schema ID produced from the ZK proof of the Key Ring creation.
    /// @param expiresAtMin The minimum expiration time produced from the ZK proof of the Key Ring creation.
    /// @param credentialGenesisIssuedAtMin The minimum credential genesis issued at time produced from the ZK proof of the Key Ring creation.
    /// @param proof The compressed Groth16 proof for the Key Ring creation.
    /// @param createUpdate The KeyRingUpdate struct representing the creation of a new Key Ring.
    /// @return keyringAddress The address of the newly created Key Ring.
    function create(
        uint256 keyringCreationNullifier,
        uint256 keyringNonce,
        uint256 proofNonce,
        uint256 sessionId,
        uint64  issuerSchemaId,
        uint64  expiresAtMin,
        uint256 credentialGenesisIssuedAtMin,
        uint256[5] calldata proof,
        KeyRingUpdate calldata createUpdate
    ) external returns (address keyringAddress);


    /// @notice A function to perform an administrative update to a Key Ring.
    /// @param keyring The address of the Key Ring to update.
    /// @param proofNonce The nonce of representing the quantity of updates to the `keyring`.
    /// @param issuerSchemaId The issuer schema ID produced from the ZK proof of the Key Ring update.
    /// @param expiresAtMin The minimum expiration time
    /// @param credentialGenesisIssuedAtMin The minimum credential genesis issued at time produced from the ZK proof of the Key Ring update.
    /// @param sessionNullifier The session nullifier produced from the ZK proof of the Key Ring update.
    /// @param proof The compressed Groth16 proof for the Key Ring update.
    /// @param keyringUpdate The KeyRingUpdate struct representing the update to perform on the Key Ring.
    function update(
        address keyring,
        uint256 proofNonce,
        uint64  issuerSchemaId,
        uint64  expiresAtMin,
        uint256 credentialGenesisIssuedAtMin,
        uint256[2] calldata sessionNullifier,
        uint256[5] calldata proof,
        KeyRingUpdate calldata keyringUpdate
    ) external;

    /// @notice Get the current [`SessionKeys[]`] associated with `keyring`.
    /// @param keyring The address of the Key Ring to query.
    /// @return sessionKeys The current session keys associated with `keyring`.
    function keyringToSessionKeys(address keyring) external view returns (SessionKey[] memory sessionKeys);

    /// @notice A function to check if a given session key is authorized for a given Key Ring.
    /// @param keyring The address of the Key Ring to check.
    /// @param key The session key to check for authorization.
    /// @return isAuthorized A boolean indicating whether the session key is authorized for the Key Ring
    function authorizedSessionKey(address keyring, SessionKey calldata key) external view returns (bool);

    /// @notice A function to retrieve the nullifier for a given Key Ring.
    /// @param keyring The address of the Key Ring to query.
    /// @return nullifier The nullifier for the given Key Ring.
    function keyringToNullifier(address keyring) external view returns (uint256);
}