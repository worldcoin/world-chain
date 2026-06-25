// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {NitroValidator} from "./vendor/nitro-validator/NitroValidator.sol";
import {ICertManager} from "./vendor/nitro-validator/ICertManager.sol";
import {CborElement, LibCborElement, CborDecode} from "./vendor/nitro-validator/CborDecode.sol";
import {LibBytes} from "./vendor/nitro-validator/LibBytes.sol";
import {INitroAttestationVerifier} from "./INitroAttestationVerifier.sol";

/// @title NitroAttestationVerifier
/// @author Worldcoin
/// @notice Fully on-chain AWS Nitro Enclaves attestation verifier built on top of
///         Base's `nitro-validator` library
///         (<https://github.com/base/nitro-validator>).
///
/// @dev This contract inherits {NitroValidator}, which performs:
///        - CBOR parsing of the COSE_Sign1 attestation document;
///        - X.509 chain validation against the pinned AWS Nitro root CA, delegated
///          to {ICertManager} (cert verifications can be amortized across calls);
///        - P-384 ECDSA verification of the COSE signature.
///
///      On top of that, this contract:
///        - rejects stale attestations (`block.timestamp - ptrs.timestamp/1000 > MAX_AGE`);
///        - matches PCR0/1/2 against caller-supplied digests (so the registry
///          interface stays bytes32-keyed even though raw Nitro PCRs are 48 bytes);
///        - returns the certified secp256k1 enclave public key.
///
///      The expected calling pattern is:
///        1. Caller invokes `NitroValidator.decodeAttestationTbs(rawDoc)` (off-chain or
///           on-chain) to split the document into `(attestationTbs, signature)`.
///        2. Caller pre-warms {ICertManager} by calling
///           `verifyCACert`/`verifyClientCert` for each cert in the cabundle in
///           separate transactions (this amortizes the ~63M-gas chain validation
///           cost across many attestations).
///        3. Caller invokes {verifyAttestation} (or registers via
///           {NitroEnclaveKeyRegistry.registerKey}, which calls this contract).
contract NitroAttestationVerifier is NitroValidator, INitroAttestationVerifier {
    using LibCborElement for CborElement;
    using LibBytes for bytes;

    /*//////////////////////////////////////////////////////////////
                                 ERRORS
    //////////////////////////////////////////////////////////////*/

    /// @notice The attestation document is older than {MAX_AGE}.
    error AttestationStale(uint256 ageSeconds);

    /// @notice The number of PCRs in the doc is less than 3 (PCR0/1/2 required).
    error InsufficientPcrs(uint256 found);

    /// @notice One of PCR0/1/2 does not match the supplied digest.
    error PcrMismatch(uint8 index);

    /// @notice The document does not embed a `public_key` field, or the embedded
    ///         key is not 65 bytes / does not start with `0x04`.
    error InvalidEmbeddedPublicKey();

    /*//////////////////////////////////////////////////////////////
                                 EVENTS
    //////////////////////////////////////////////////////////////*/

    /// @notice Emitted when an attestation has been fully verified on-chain.
    /// @param attestationDigest keccak256 of the verified `attestationTbs` bytes.
    /// @param publicKey         The 65-byte SEC1-uncompressed enclave public key.
    /// @param pcr0              keccak256 of raw PCR0.
    /// @param pcr1              keccak256 of raw PCR1.
    /// @param pcr2              keccak256 of raw PCR2.
    /// @param timestamp         The `timestamp` field from the attestation document.
    event AttestationVerified(
        bytes32 indexed attestationDigest,
        bytes publicKey,
        bytes32 pcr0,
        bytes32 pcr1,
        bytes32 pcr2,
        uint64 timestamp
    );

    /*//////////////////////////////////////////////////////////////
                              CONSTANTS
    //////////////////////////////////////////////////////////////*/

    /// @notice Maximum age, in seconds, of an attestation document for it to be
    ///         considered fresh. AWS Nitro attestation documents include an
    ///         NSM-signed timestamp; rejecting old ones prevents naive replay.
    uint256 public constant MAX_AGE = 60 minutes;

    /*//////////////////////////////////////////////////////////////
                              CONSTRUCTOR
    //////////////////////////////////////////////////////////////*/

    /// @param certManager_ Address of the pre-deployed {CertManager} that caches
    ///                     verified certificates from the AWS Nitro PKI.
    constructor(ICertManager certManager_) NitroValidator(certManager_) {}

    /*//////////////////////////////////////////////////////////////
                            VERIFICATION
    //////////////////////////////////////////////////////////////*/

    /// @inheritdoc INitroAttestationVerifier
    /// @dev Side effects: this is not `view` because the underlying
    ///      {NitroValidator.validateAttestation} writes verified cert entries to
    ///      {ICertManager}'s cache when uncached certs are encountered.
    function verifyAttestation(
        bytes calldata attestationTbs,
        bytes calldata signature,
        bytes32 pcr0,
        bytes32 pcr1,
        bytes32 pcr2
    ) external returns (bytes memory publicKey) {
        // 1. Full on-chain COSE_Sign1 + cert chain + P-384 verification.
        Ptrs memory ptrs = validateAttestation(attestationTbs, signature);

        // 2. Freshness check. ptrs.timestamp is milliseconds since the Unix epoch.
        uint256 docSeconds = uint256(ptrs.timestamp) / 1000;
        if (block.timestamp > docSeconds && block.timestamp - docSeconds > MAX_AGE) {
            revert AttestationStale(block.timestamp - docSeconds);
        }

        // 3. PCR0/1/2 match. We hash the raw PCR bytes (48 bytes for SHA-384) so
        //    that callers can use a stable bytes32 identifier.
        if (ptrs.pcrs.length < 3) revert InsufficientPcrs(ptrs.pcrs.length);
        _requirePcr(attestationTbs, ptrs.pcrs[0], pcr0, 0);
        _requirePcr(attestationTbs, ptrs.pcrs[1], pcr1, 1);
        _requirePcr(attestationTbs, ptrs.pcrs[2], pcr2, 2);

        // 4. Extract the certified enclave public key.
        if (ptrs.publicKey.isNull() || ptrs.publicKey.length() != 65) {
            revert InvalidEmbeddedPublicKey();
        }
        // `attestationTbs` is calldata; LibBytes.slice operates on `bytes memory`,
        // so we copy via abi.encodePacked.
        bytes memory tbsMem = attestationTbs;
        publicKey = tbsMem.slice(ptrs.publicKey.start(), ptrs.publicKey.length());
        if (publicKey[0] != 0x04) revert InvalidEmbeddedPublicKey();

        emit AttestationVerified(
            keccak256(attestationTbs), publicKey, pcr0, pcr1, pcr2, ptrs.timestamp
        );
    }

    /*//////////////////////////////////////////////////////////////
                                INTERNAL
    //////////////////////////////////////////////////////////////*/

    /// @dev Slices the raw PCR bytes out of `tbs` and asserts
    ///      `keccak256(rawPcr) == expected`.
    function _requirePcr(
        bytes calldata tbs,
        CborElement pcrPtr,
        bytes32 expected,
        uint8 index
    ) internal pure {
        uint256 start = pcrPtr.start();
        uint256 len = pcrPtr.length();
        bytes32 hash;
        // keccak256 over the calldata slice without copying to memory.
        assembly ("memory-safe") {
            let ptr := mload(0x40)
            calldatacopy(ptr, add(tbs.offset, start), len)
            hash := keccak256(ptr, len)
        }
        if (hash != expected) revert PcrMismatch(index);
    }
}
