// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {INitroAttestationVerifier} from "./INitroAttestationVerifier.sol";

/// @title NitroEnclaveKeyRegistry
/// @author Worldcoin
/// @notice Registry of AWS Nitro enclave public keys, indexed by PCR0/PCR1/PCR2.
/// @dev Keys are registered after their attestation document has been verified by an
///      {INitroAttestationVerifier}. The owner may revoke any registered key.
///
///      A `(pcr0, pcr1, pcr2)` triple uniquely identifies an enclave image. Different
///      enclave launches of the same image produce different ephemeral keys, so the
///      registry stores only the *most recent* key for a given PCR triple. The
///      previous key for that triple is implicitly superseded — call sites that need
///      to detect rotation should listen for {KeyRegistered} events.
contract NitroEnclaveKeyRegistry is Ownable {
    /*//////////////////////////////////////////////////////////////
                                 ERRORS
    //////////////////////////////////////////////////////////////*/

    /// @notice Thrown when the attestation verifier rejects the supplied document.
    error AttestationRejected();

    /// @notice Thrown when {revokeKey} is called for a key that is not registered.
    error KeyNotRegistered();

    /// @notice Thrown when an empty public key is supplied.
    error EmptyPublicKey();

    /*//////////////////////////////////////////////////////////////
                                 EVENTS
    //////////////////////////////////////////////////////////////*/

    /// @notice Emitted when an enclave public key is registered against a PCR triple.
    event KeyRegistered(bytes publicKey, bytes32 pcr0, bytes32 pcr1, bytes32 pcr2);

    /// @notice Emitted when a previously registered key is revoked by the owner.
    event KeyRevoked(bytes publicKey);

    /*//////////////////////////////////////////////////////////////
                                STORAGE
    //////////////////////////////////////////////////////////////*/

    /// @notice Attestation verifier used to gate registration.
    INitroAttestationVerifier public immutable verifier;

    /// @notice keccak256(publicKey) => registered flag. Cleared on revoke.
    mapping(bytes32 keyHash => bool registered) private _registered;

    /// @notice keccak256(abi.encode(pcr0, pcr1, pcr2)) => current public key for the
    ///         enclave image identified by those PCRs.
    mapping(bytes32 pcrHash => bytes publicKey) private _keyByPCRs;

    /*//////////////////////////////////////////////////////////////
                              CONSTRUCTOR
    //////////////////////////////////////////////////////////////*/

    /// @param verifier_ Attestation verifier used to validate documents at
    ///                  registration time.
    /// @param owner_    Initial owner allowed to revoke keys.
    constructor(INitroAttestationVerifier verifier_, address owner_) Ownable(owner_) {
        verifier = verifier_;
    }

    /*//////////////////////////////////////////////////////////////
                             REGISTRATION
    //////////////////////////////////////////////////////////////*/

    /// @notice Registers `publicKey` as the enclave key bound to the given PCRs by
    ///         `attestationDoc`.
    /// @dev The call reverts unless {INitroAttestationVerifier.verifyAttestation}
    ///      returns `true` for the supplied tuple. If a key was already registered
    ///      for the same PCR triple, it is superseded (the old key remains in
    ///      {isKeyRegistered} until revoked).
    ///
    /// @param attestationDoc Raw NSM/COSE_Sign1 attestation document bytes.
    /// @param publicKey      65-byte SEC1-uncompressed secp256k1 enclave key.
    /// @param pcr0           PCR0 of the attested image.
    /// @param pcr1           PCR1 of the attested image.
    /// @param pcr2           PCR2 of the attested image.
    function registerKey(
        bytes memory attestationDoc,
        bytes calldata publicKey,
        bytes32 pcr0,
        bytes32 pcr1,
        bytes32 pcr2
    ) external {
        if (publicKey.length == 0) revert EmptyPublicKey();

        if (!verifier.verifyAttestation(attestationDoc, publicKey, pcr0, pcr1, pcr2)) {
            revert AttestationRejected();
        }

        _registered[keccak256(publicKey)] = true;
        _keyByPCRs[_pcrHash(pcr0, pcr1, pcr2)] = publicKey;

        emit KeyRegistered(publicKey, pcr0, pcr1, pcr2);
    }

    /*//////////////////////////////////////////////////////////////
                               REVOCATION
    //////////////////////////////////////////////////////////////*/

    /// @notice Revokes a previously registered enclave key.
    /// @dev Only callable by the owner. Does not clear the key stored in
    ///      {getKeyByPCRs} — callers must additionally consult {isKeyRegistered}
    ///      before trusting the returned bytes.
    function revokeKey(bytes calldata publicKey) external onlyOwner {
        bytes32 keyHash = keccak256(publicKey);
        if (!_registered[keyHash]) revert KeyNotRegistered();
        _registered[keyHash] = false;
        emit KeyRevoked(publicKey);
    }

    /*//////////////////////////////////////////////////////////////
                                 VIEWS
    //////////////////////////////////////////////////////////////*/

    /// @notice Returns whether `publicKey` is currently registered (and not revoked).
    function isKeyRegistered(bytes calldata publicKey) external view returns (bool) {
        return _registered[keccak256(publicKey)];
    }

    /// @notice Returns the latest registered key for a given PCR triple, or an empty
    ///         byte string if none has been registered.
    function getKeyByPCRs(bytes32 pcr0, bytes32 pcr1, bytes32 pcr2)
        external
        view
        returns (bytes memory publicKey)
    {
        return _keyByPCRs[_pcrHash(pcr0, pcr1, pcr2)];
    }

    /*//////////////////////////////////////////////////////////////
                                INTERNAL
    //////////////////////////////////////////////////////////////*/

    function _pcrHash(bytes32 pcr0, bytes32 pcr1, bytes32 pcr2) internal pure returns (bytes32) {
        return keccak256(abi.encode(pcr0, pcr1, pcr2));
    }
}
