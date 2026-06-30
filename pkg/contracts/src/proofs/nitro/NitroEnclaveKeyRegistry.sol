// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {INitroAttestationVerifier} from "./INitroAttestationVerifier.sol";

/// @title NitroEnclaveKeyRegistry
/// @author Worldcoin
/// @notice Registry of attested AWS Nitro enclave secp256k1 public keys.
/// @dev Registration goes through a fully on-chain `INitroAttestationVerifier`
///      which:
///        - validates the COSE_Sign1 P-384 signature;
///        - validates the X.509 cert chain to the AWS Nitro root CA (via the
///          cached `ICertManager`);
///        - extracts PCR0/1/2 from the document and checks they correspond to
///          an enclave image that the verifier's owner has explicitly approved;
///        - returns the embedded SEC1-uncompressed secp256k1 public key together
///          with the PCR triple it was bound to.
///
///      The registry records the returned key in a per-key `keccak256(publicKey)`
///      flag and emits `KeyRegistered` with the bound PCR triple, which is the
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

    /// @notice Thrown when `revokeKey` is called for a key that is not
    ///         currently registered as {KeyStatus.Active}.
    error KeyNotRegistered();

    /// @notice Thrown when `registerKey` is called for a key that is already
    ///         {KeyStatus.Active}.
    error KeyAlreadyRegistered();

    /// @notice Thrown when `registerKey` is called for a key that was
    ///         previously revoked. Revocation is permanent — a compromised
    ///         enclave key must not be silently restored by re-submitting
    ///         its attestation document.
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
                                  TYPES
    //////////////////////////////////////////////////////////////*/

    /// @notice Lifecycle state of a registered key.
    /// @dev The default zero value (`Unknown`) denotes a key that has never
    ///      been registered. Transitions are strictly
    ///      `Unknown → Active → Revoked`; there is no path back.
    enum KeyStatus {
        Unknown,
        Active,
        Revoked
    }

    /*//////////////////////////////////////////////////////////////
                                STORAGE
    //////////////////////////////////////////////////////////////*/

    /// @notice On-chain Nitro attestation verifier.
    INitroAttestationVerifier public immutable verifier;

    /// @notice `keccak256(publicKey) → lifecycle status`.
    mapping(bytes32 keyHash => KeyStatus status) private _keyStatus;

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
    /// @param attestationSigHints Off-chain modular-inverse hints for the
    ///                       P-384 attestation signature. Pre-compute with
    ///                       `tools/p384_hints.js attestation ...`.
    function registerKey(
        bytes calldata attestationTbs,
        bytes calldata signature,
        bytes calldata attestationSigHints
    ) external returns (bytes memory publicKey, bytes32 pcr0, bytes32 pcr1, bytes32 pcr2) {
        (publicKey, pcr0, pcr1, pcr2) = verifier.verifyAttestation(attestationTbs, signature, attestationSigHints);
        if (publicKey.length != 65 || publicKey[0] != 0x04) revert InvalidPublicKey();

        bytes32 keyHash = keccak256(publicKey);
        KeyStatus status = _keyStatus[keyHash];
        if (status == KeyStatus.Revoked) revert KeyRevokedPermanently();
        if (status == KeyStatus.Active) revert KeyAlreadyRegistered();
        _keyStatus[keyHash] = KeyStatus.Active;

        emit KeyRegistered(publicKey, pcr0, pcr1, pcr2);
    }

    /*//////////////////////////////////////////////////////////////
                               REVOCATION
    //////////////////////////////////////////////////////////////*/

    /// @notice Revoke a previously registered enclave key.
    /// @dev Only callable by the owner. Revocation is permanent — see
    ///      `isKeyRevoked` — so a compromised key cannot be silently restored
    ///      by replaying its attestation document.
    ///
    ///      ## Relationship to {NitroAttestationVerifier.revokePCRSet}
    ///      Revoking a PCR set on the verifier (i.e. retiring an enclave
    ///      image) does **not** automatically transition the keys that were
    ///      registered under that image to {KeyStatus.Revoked} here. Each key
    ///      remains {KeyStatus.Active} until `revokeKey` is called for it
    ///      individually.
    ///
    ///      This is intentional. Nitro enclave signing keys are ephemeral:
    ///      they are generated in-memory at startup, never persisted to
    ///      disk, and destroyed the moment the enclave process exits. The
    ///      designed incident-response flow for a compromised image is:
    ///        1. Stop the running enclave instances (the AWS Nitro hardware
    ///           isolation guarantees the key is destroyed with the process).
    ///        2. Call {NitroAttestationVerifier.revokePCRSet} so no fresh
    ///           enclave from the same image can re-register.
    ///      The two steps together eliminate the threat without per-key
    ///      cascading on-chain.
    ///
    ///      Belt-and-suspenders operators can still observe
    ///      {NitroAttestationVerifier.PCRSetRevoked} events off-chain and
    ///      call `revokeKey` for every affected key. The `KeyRegistered`
    ///      event carries the bound PCR triple specifically to make this
    ///      easy.
    ///
    ///      ## Why no on-chain cascade?
    ///      An automatic on-chain cascade was considered and rejected:
    ///        - Storing `pcrSetHash → keyHash[]` to enumerate affected keys
    ///          requires an unbounded array per image, with O(N) gas on
    ///          `registerKey` and on the cascade itself.
    ///        - Doing the lookup lazily in `isKeyRegistered` would add an
    ///          extra SLOAD on every proof-verification call (the hot path),
    ///          for no security gain given Nitro's hardware key-destruction
    ///          guarantee.
    function revokeKey(bytes calldata publicKey) external onlyOwner {
        bytes32 keyHash = keccak256(publicKey);
        if (_keyStatus[keyHash] != KeyStatus.Active) revert KeyNotRegistered();
        _keyStatus[keyHash] = KeyStatus.Revoked;
        emit KeyRevoked(publicKey);
    }

    /*//////////////////////////////////////////////////////////////
                                 VIEWS
    //////////////////////////////////////////////////////////////*/

    /// @notice Returns the lifecycle status of `publicKey`.
    function keyStatus(bytes calldata publicKey) external view returns (KeyStatus) {
        return _keyStatus[keccak256(publicKey)];
    }

    /// @notice Returns whether `publicKey` is currently registered (and not revoked).
    function isKeyRegistered(bytes calldata publicKey) external view returns (bool) {
        return _keyStatus[keccak256(publicKey)] == KeyStatus.Active;
    }

    /// @notice Returns whether `publicKey` has been permanently revoked.
    function isKeyRevoked(bytes calldata publicKey) external view returns (bool) {
        return _keyStatus[keccak256(publicKey)] == KeyStatus.Revoked;
    }
}
