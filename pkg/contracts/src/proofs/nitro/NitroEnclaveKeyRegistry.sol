// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {INitroAttestationVerifier} from "./INitroAttestationVerifier.sol";

/// @title NitroEnclaveKeyRegistry
/// @author Worldcoin
/// @notice Registry of attested AWS Nitro enclave secp256k1 public keys.
/// @dev Registration goes through a fully on-chain {INitroAttestationVerifier}
///      which:
///        - validates the COSE_Sign1 P-384 signature;
///        - validates the X.509 cert chain to the AWS Nitro root CA (via the
///          cached {ICertManager});
///        - extracts PCR0/1/2 from the document and checks they correspond to
///          an enclave image that the verifier's owner has explicitly approved;
///        - returns the embedded SEC1-uncompressed secp256k1 public key together
///          with the PCR triple it was bound to.
///
///      The registry records the returned key in a per-key `keccak256(publicKey)`
///      flag and emits {KeyRegistered} with the bound PCR triple, which is the
///      authoritative off-chain index from PCRs to keys. The registry
///      deliberately does NOT maintain an on-chain `PCRs → key` lookup:
///      multiple validator instances run the same enclave image simultaneously,
///      so the relationship is one-to-many and any 1:1 map would silently
///      overwrite peers' entries.
///
///      The registry owner can revoke any key at any time (e.g. on enclave
///      compromise); revocation is permanent — a revoked key cannot be
///      re-registered even if a valid attestation document for it is replayed.
contract NitroEnclaveKeyRegistry is Ownable {
    /*//////////////////////////////////////////////////////////////
                                 ERRORS
    //////////////////////////////////////////////////////////////*/

    /// @notice Thrown when {revokeKey} is called for a key that is not registered.
    error KeyNotRegistered();

    /// @notice Thrown when {registerKey} is called for a key that was previously
    ///         revoked. Revocation is permanent — a compromised enclave key must
    ///         not be silently restored by re-submitting its attestation document.
    error KeyRevokedPermanently();

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

    /// @notice keccak256(publicKey) => permanently revoked flag. Set on revoke and
    ///         never cleared, so a revoked key cannot be re-registered even if a
    ///         valid attestation document for it is replayed.
    mapping(bytes32 keyHash => bool revoked) private _revoked;

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

    /// @notice Verify an AWS Nitro attestation document on-chain and register
    ///         the enclave public key it certifies. The PCR triple bound to
    ///         the document is checked against the verifier's allowlist; this
    ///         function does not take any PCR parameters.
    ///
    /// @dev The full COSE_Sign1 / X.509 / P-384 verification, the PCR
    ///      allowlist check, and the freshness check are all delegated to
    ///      {INitroAttestationVerifier.verifyAttestation}; this function only
    ///      stores the resulting key. Reverts on any verification failure.
    ///
    /// @param attestationTbs The COSE_Sign1 TBS bytes (from
    ///                       `NitroValidator.decodeAttestationTbs`).
    /// @param signature      The 96-byte (r||s) P-384 attestation signature.
    function registerKey(bytes calldata attestationTbs, bytes calldata signature)
        external
        returns (bytes memory publicKey, bytes32 pcr0, bytes32 pcr1, bytes32 pcr2)
    {
        (publicKey, pcr0, pcr1, pcr2) = verifier.verifyAttestation(attestationTbs, signature);
        if (publicKey.length != 65 || publicKey[0] != 0x04) revert InvalidPublicKey();

        bytes32 keyHash = keccak256(publicKey);
        if (_revoked[keyHash]) revert KeyRevokedPermanently();
        _registered[keyHash] = true;

        emit KeyRegistered(publicKey, pcr0, pcr1, pcr2);
    }

    /*//////////////////////////////////////////////////////////////
                               REVOCATION
    //////////////////////////////////////////////////////////////*/

    /// @notice Revoke a previously registered enclave key.
    /// @dev Only callable by the owner. Revocation is permanent — see
    ///      {isKeyRevoked} — so a compromised key cannot be silently restored
    ///      by replaying its attestation document.
    function revokeKey(bytes calldata publicKey) external onlyOwner {
        bytes32 keyHash = keccak256(publicKey);
        if (!_registered[keyHash]) revert KeyNotRegistered();
        _registered[keyHash] = false;
        _revoked[keyHash] = true;
        emit KeyRevoked(publicKey);
    }

    /*//////////////////////////////////////////////////////////////
                                 VIEWS
    //////////////////////////////////////////////////////////////*/

    /// @notice Returns whether `publicKey` is currently registered (and not revoked).
    function isKeyRegistered(bytes calldata publicKey) external view returns (bool) {
        return _registered[keccak256(publicKey)];
    }

    /// @notice Returns whether `publicKey` has been permanently revoked.
    function isKeyRevoked(bytes calldata publicKey) external view returns (bool) {
        return _revoked[keccak256(publicKey)];
    }
}
