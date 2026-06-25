// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {INitroAttestationVerifier} from "./INitroAttestationVerifier.sol";

/// @title NitroEnclaveKeyRegistry
/// @author Worldcoin
/// @notice Registry of AWS Nitro enclave secp256k1 public keys, indexed by
///         PCR0/PCR1/PCR2 measurements.
/// @dev Registration goes through a fully on-chain {INitroAttestationVerifier} which:
///        - validates the COSE_Sign1 P-384 signature;
///        - validates the X.509 cert chain to the AWS Nitro root CA (via the cached
///          {ICertManager});
///        - confirms PCR0/1/2 match the supplied digests;
///        - returns the embedded SEC1-uncompressed secp256k1 public key.
///
///      The registry stores the returned key keyed by PCR triple and exposes views
///      for the verifier-side proof contracts ({NitroProofVerifier}). The owner can
///      revoke any key at any time (e.g. on enclave compromise).
contract NitroEnclaveKeyRegistry is Ownable {
    /*//////////////////////////////////////////////////////////////
                                 ERRORS
    //////////////////////////////////////////////////////////////*/

    /// @notice Thrown when {revokeKey} is called for a key that is not registered.
    error KeyNotRegistered();

    /// @notice Thrown when the verifier returns a malformed public key.
    error InvalidPublicKey();

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

    /// @notice On-chain Nitro attestation verifier.
    INitroAttestationVerifier public immutable verifier;

    /// @notice keccak256(publicKey) => registered flag. Cleared on revoke.
    mapping(bytes32 keyHash => bool registered) private _registered;

    /// @notice keccak256(abi.encode(pcr0, pcr1, pcr2)) => current public key for the
    ///         enclave image identified by those PCRs.
    mapping(bytes32 pcrHash => bytes publicKey) private _keyByPCRs;

    /*//////////////////////////////////////////////////////////////
                              CONSTRUCTOR
    //////////////////////////////////////////////////////////////*/

    /// @param verifier_ The Nitro attestation verifier used to validate
    ///                  attestation documents on-chain at registration time.
    /// @param owner_    Initial owner allowed to revoke keys.
    constructor(INitroAttestationVerifier verifier_, address owner_) Ownable(owner_) {
        verifier = verifier_;
    }

    /*//////////////////////////////////////////////////////////////
                             REGISTRATION
    //////////////////////////////////////////////////////////////*/

    /// @notice Verify an AWS Nitro attestation document on-chain and register the
    ///         enclave public key it certifies.
    ///
    /// @dev The full COSE_Sign1 / X.509 / P-384 verification is delegated to
    ///      {INitroAttestationVerifier.verifyAttestation}; this function only
    ///      stores the resulting key. Reverts on any verification failure.
    ///
    /// @param attestationTbs The COSE_Sign1 TBS bytes (from
    ///                       `NitroValidator.decodeAttestationTbs`).
    /// @param signature      The 96-byte (r||s) P-384 attestation signature.
    /// @param pcr0           Expected `keccak256(rawPcr0)`.
    /// @param pcr1           Expected `keccak256(rawPcr1)`.
    /// @param pcr2           Expected `keccak256(rawPcr2)`.
    function registerKey(
        bytes calldata attestationTbs,
        bytes calldata signature,
        bytes32 pcr0,
        bytes32 pcr1,
        bytes32 pcr2
    ) external returns (bytes memory publicKey) {
        publicKey = verifier.verifyAttestation(attestationTbs, signature, pcr0, pcr1, pcr2);
        if (publicKey.length != 65 || publicKey[0] != 0x04) revert InvalidPublicKey();

        _registered[keccak256(publicKey)] = true;
        _keyByPCRs[_pcrHash(pcr0, pcr1, pcr2)] = publicKey;

        emit KeyRegistered(publicKey, pcr0, pcr1, pcr2);
    }

    /*//////////////////////////////////////////////////////////////
                               REVOCATION
    //////////////////////////////////////////////////////////////*/

    /// @notice Revoke a previously registered enclave key.
    /// @dev Only callable by the owner. Does not clear the key returned by
    ///      {getKeyByPCRs} — callers must additionally consult {isKeyRegistered}
    ///      before trusting that key.
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

    /// @notice Returns the latest registered key for a given PCR triple, or an
    ///         empty byte string if none has been registered.
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
