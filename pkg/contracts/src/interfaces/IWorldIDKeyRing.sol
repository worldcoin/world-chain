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
    /// @param keyringNullifier The creation nullifier produced from the OPRF proof of the Key Ring creation.
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


    function update(
        address keyring,
        uint256 proofNonce,
        uint64  issuerSchemaId,
        uint64  expiresAtMin,
        uint256 credentialGenesisIssuedAtMin,
        uint256[2] calldata sessionNullifier,
        uint256[5] calldata proof,           // compressed Groth16 proof
        KeyRingUpdate calldata keyringUpdate
    ) external;

    function getSessionKeys(address keyring) external view returns (SessionKey[] memory);
    function isAuthorized(address keyring, SessionKey calldata key) external view returns (bool);
    function getKeyringNullifier(address keyring) external view returns (uint256);
}