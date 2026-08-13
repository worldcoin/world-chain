// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {INitroAttestationVerifier} from "../interfaces/INitroAttestationVerifier.sol";

/// @title NitroEnclaveKeyRegistry
/// @author World Contributors
/// @custom:security-contact security@toolsforhumanity.com
contract NitroEnclaveKeyRegistry is Ownable {
    /*//////////////////////////////////////////////////////////////
                                 ERRORS
    //////////////////////////////////////////////////////////////*/

    /// @notice Thrown when `revokeSigner` is called for an address that is not active.
    error SignerNotRegistered();

    /// @notice Thrown when `registerKey` derives a signer that is already active.
    error SignerAlreadyRegistered();

    /// @notice Thrown when `registerKey` is called for a key that was
    ///         previously revoked.
    error SignerRevokedPermanently();

    /// @notice Thrown when the verifier returns a malformed public key.
    error InvalidPublicKey();

    /*//////////////////////////////////////////////////////////////
                                 EVENTS
    //////////////////////////////////////////////////////////////*/

    /// @notice Emitted when an enclave signer is registered against a PCR triple.
    event SignerRegistered(address indexed signer, bytes32 pcr0, bytes32 pcr1, bytes32 pcr2);

    /// @notice Emitted when a previously registered signer is revoked by the owner.
    event SignerRevoked(address indexed signer);

    /*//////////////////////////////////////////////////////////////
                                  TYPES
    //////////////////////////////////////////////////////////////*/

    /// @notice Lifecycle state of a registered signer.
    enum SignerStatus {
        Unknown,
        Active,
        Revoked
    }

    /*//////////////////////////////////////////////////////////////
                                STORAGE
    //////////////////////////////////////////////////////////////*/

    /// @notice On-chain Nitro attestation verifier.
    INitroAttestationVerifier public immutable verifier;

    /// @notice Enclave signer address to lifecycle status.
    mapping(address signer => SignerStatus status) private _signerStatus;

    /// @notice Enclave signer address to the PCR0 image ID proven at registration.
    mapping(address signer => bytes32 imageId) public signerImageId;

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
    ///      `INitroAttestationVerifier.verifyAttestation`; this function only
    ///      stores the resulting key. Reverts on any verification failure.
    ///
    /// @param attestationTbs The COSE_Sign1 TBS bytes (from
    ///                       `NitroValidator.decodeAttestationTbs`).
    /// @param signature      The 96-byte (r||s) P-384 attestation signature.
    /// @param attestationSigHints Off-chain modular-inverse hints for the
    ///                       P-384 attestation signature. Pre-compute with
    ///                       `tools/p384_hints.js attestation ...`.
    function registerKey(bytes calldata attestationTbs, bytes calldata signature, bytes calldata attestationSigHints)
        external
        returns (address signer, bytes32 pcr0, bytes32 pcr1, bytes32 pcr2)
    {
        bytes memory publicKey;
        (publicKey, pcr0, pcr1, pcr2) = verifier.verifyAttestation(attestationTbs, signature, attestationSigHints);
        if (publicKey.length != 65 || publicKey[0] != 0x04) revert InvalidPublicKey();

        signer = _signerAddress(publicKey);
        if (signer == address(0)) revert InvalidPublicKey();
        SignerStatus status = _signerStatus[signer];
        if (status == SignerStatus.Revoked) revert SignerRevokedPermanently();
        if (status == SignerStatus.Active) revert SignerAlreadyRegistered();
        signerImageId[signer] = pcr0;
        _signerStatus[signer] = SignerStatus.Active;

        emit SignerRegistered(signer, pcr0, pcr1, pcr2);
    }

    /*//////////////////////////////////////////////////////////////
                               REVOCATION
    //////////////////////////////////////////////////////////////*/

    /// @notice Revoke a previously registered enclave signer.
    /// @dev Revocation is permanent because registration is permissionless: while an
    ///      attestation is still fresh, deleting the signer would allow the same document to
    ///      register it again. Normal enclave shutdown does not require revocation because the
    ///      ephemeral key is destroyed. Use this function when the signer key may be compromised.
    ///      PCR-set revocation separately prevents future registrations for an image and does
    ///      not enumerate or revoke its existing signers.
    function revokeSigner(address signer) external onlyOwner {
        if (_signerStatus[signer] != SignerStatus.Active) revert SignerNotRegistered();
        _signerStatus[signer] = SignerStatus.Revoked;
        emit SignerRevoked(signer);
    }

    /*//////////////////////////////////////////////////////////////
                                 VIEWS
    //////////////////////////////////////////////////////////////*/

    /// @notice Returns the lifecycle status of `signer`.
    function signerStatus(address signer) external view returns (SignerStatus) {
        return _signerStatus[signer];
    }

    /// @notice Returns whether `signer` is currently registered and not revoked.
    function isSignerRegistered(address signer) external view returns (bool) {
        return _signerStatus[signer] == SignerStatus.Active;
    }

    /// @notice Returns whether `signer` is active and was attested for `imageId`.
    function isSignerRegisteredForImage(address signer, bytes32 imageId) external view returns (bool) {
        return _signerStatus[signer] == SignerStatus.Active && signerImageId[signer] == imageId;
    }

    /// @notice Returns whether `signer` has been permanently revoked.
    function isSignerRevoked(address signer) external view returns (bool) {
        return _signerStatus[signer] == SignerStatus.Revoked;
    }

    /// @dev Ethereum addresses hash the 64-byte X/Y coordinates, excluding the 0x04 SEC1 prefix.
    function _signerAddress(bytes memory publicKey) internal pure returns (address signer) {
        bytes32 keyHash;
        assembly ("memory-safe") {
            keyHash := keccak256(add(publicKey, 33), 64)
        }
        signer = address(uint160(uint256(keyHash)));
    }
}
