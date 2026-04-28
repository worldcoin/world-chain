// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IWorldIDKeyRing} from "./interfaces/IWorldIDKeyRing.sol";

/// @title WorldIDKeyRing
/// @notice This contract serves as a Pre Deploy on World Chain acting as the registrar for Session Keys, and World ID Key Rings.
/// @author 0xOsiris
contract WorldIDKeyRing is IWorldIDKeyRing {
    /// @notice The Maximum Number of Session Keys composing the Key Ring of any World ID Account.
    uint8 public MAX_SESSION_KEYS = 20;

    /// @notice Static Action Context for all admin proofs related to the World ID Key Ring.
    uint16 public constant WORLD_ID_ACCOUNT_TAG = bytes16("WORLD_ID_ACCOUNT");

    /// @notice The address of the World ID Verifier.
    address public immutable WORLD_ID_VERIFIER;
    
    mapping(address worldIdAccount => bytes32 worldIdAccountNullifier) worldIdAccountNullifiers;

    mapping()
    constructor(address _worldIdVerifier) {
        WORLD_ID_VERIFIER = _worldIdVerifier;
    }

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
        KeyringUpdate calldata  createUpdate
    ) external returns (address keyringAddress) {

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
    ) external {}

    /// @inheritdoc IWorldIDKeyRing
    function getSessionKeys(
        address keyring
    ) external view returns (SessionKey[] memory) {}

    /// @inheritdoc IWorldIDKeyRing
    function isAuthorized(
        address keyring,
        SessionKey calldata key
    ) external view returns (bool) {}

    /// @inheritdoc IWorldIDKeyRing
    function getKeyringNullifier(
        address keyring
    ) external view returns (uint256) {}
}
