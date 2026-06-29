// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {NitroValidator} from "@nitro-validator/NitroValidator.sol";
import {ICertManager} from "@nitro-validator/ICertManager.sol";
import {CborElement, LibCborElement, CborDecode} from "@nitro-validator/CborDecode.sol";
import {LibBytes} from "@nitro-validator/LibBytes.sol";
import {INitroAttestationVerifier} from "./INitroAttestationVerifier.sol";

/// @title NitroAttestationVerifier
/// @author Worldcoin
/// @notice Fully on-chain AWS Nitro Enclaves attestation verifier built on top
///         of Base's `nitro-validator` library
///         (<https://github.com/base/nitro-validator>) with an owner-managed
///         allowlist of approved enclave images (PCR triples).
///
/// @dev This contract inherits {NitroValidator}, which performs:
///        - CBOR parsing of the COSE_Sign1 attestation document;
///        - X.509 chain validation against the pinned AWS Nitro root CA,
///          delegated to {ICertManager} (cert verifications can be amortized
///          across calls);
///        - P-384 ECDSA verification of the COSE signature.
///
///      On top of that, this contract:
///        - rejects stale attestations (older than {MAX_AGE}) and attestations
///          dated more than {CLOCK_SKEW_TOLERANCE} in the future;
///        - extracts the PCR0/1/2 triple embedded in the document, hashes each
///          raw 48-byte PCR with keccak256, and asserts the resulting
///          `(pcr0, pcr1, pcr2)` triple is in the owner-managed allowlist
///          {approvedPCRSets};
///        - returns the certified secp256k1 enclave public key together with
///          the triple it was bound to.
///
///      ## Allowlist & upgrade flow
///
///      The owner maintains a set of approved PCR triples. Each triple
///      identifies one enclave image (EIF). Adding a new image is done via
///      {approvePCRSet}; retiring an old one is done via {revokePCRSet}. The
///      registry stores enclave keys keyed by PCR triple and accepts new
///      attestations only for triples that are *currently* approved, so the
///      operator upgrade flow is:
///        1. Build a new EIF and capture its PCR0/1/2 measurements.
///        2. `approvePCRSet(newPcr0, newPcr1, newPcr2)`.
///        3. Roll out new enclaves; each registers its ephemeral key via
///           {NitroEnclaveKeyRegistry.registerKey}, which delegates here.
///        4. Once the migration completes, `revokePCRSet(oldPcr0, oldPcr1,
///           oldPcr2)` to disallow future registrations for the old image.
///           Already-registered keys remain valid until separately revoked in
///           the registry.
///
///      ## Expected calling pattern
///        1. Caller invokes `NitroValidator.decodeAttestationTbs(rawDoc)`
///           (off-chain or on-chain) to split the document into
///           `(attestationTbs, signature)`.
///        2. Caller pre-warms {ICertManager} by calling
///           `verifyCACert`/`verifyClientCert` for each cert in the cabundle
///           in separate transactions (this amortizes the ~63M-gas chain
///           validation cost across many attestations).
///        3. Caller invokes {verifyAttestation} (typically via
///           {NitroEnclaveKeyRegistry.registerKey}).
contract NitroAttestationVerifier is NitroValidator, INitroAttestationVerifier, Ownable {
    using LibCborElement for CborElement;
    using LibBytes for bytes;

    /*//////////////////////////////////////////////////////////////
                                 ERRORS
    //////////////////////////////////////////////////////////////*/

    /// @notice The attestation document is older than {MAX_AGE}.
    error AttestationStale(uint256 ageSeconds);

    /// @notice The attestation document is dated more than
    ///         {CLOCK_SKEW_TOLERANCE} seconds in the future. Without this
    ///         guard a forged future timestamp would be perpetually fresh.
    error AttestationFromFuture(uint256 driftSeconds);

    /// @notice The number of PCRs in the doc is less than 3 (PCR0/1/2
    ///         required).
    error InsufficientPcrs(uint256 found);

    /// @notice The PCR triple extracted from the document is not currently
    ///         approved by the owner.
    error PCRSetNotApproved(bytes32 pcr0, bytes32 pcr1, bytes32 pcr2);

    /// @notice The document does not embed a `public_key` field, or the
    ///         embedded key is not a 65-byte SEC1-uncompressed
    ///         (`0x04 || X || Y`) secp256k1 key. The World Nitro enclave is
    ///         expected to emit its ephemeral key in uncompressed form.
    error InvalidEmbeddedPublicKey();

    /*//////////////////////////////////////////////////////////////
                                 EVENTS
    //////////////////////////////////////////////////////////////*/

    /// @notice Emitted when a PCR triple is added to the allowlist.
    event PCRSetApproved(bytes32 pcr0, bytes32 pcr1, bytes32 pcr2);

    /// @notice Emitted when a PCR triple is removed from the allowlist.
    event PCRSetRevoked(bytes32 pcr0, bytes32 pcr1, bytes32 pcr2);

    /// @notice Emitted when an attestation has been fully verified on-chain.
    /// @param attestationDigest keccak256 of the verified `attestationTbs`.
    /// @param publicKey         The 65-byte SEC1-uncompressed enclave key.
    /// @param pcr0              keccak256 of raw PCR0.
    /// @param pcr1              keccak256 of raw PCR1.
    /// @param pcr2              keccak256 of raw PCR2.
    /// @param timestamp         The `timestamp` field from the attestation.
    event AttestationVerified(
        bytes32 indexed attestationDigest, bytes publicKey, bytes32 pcr0, bytes32 pcr1, bytes32 pcr2, uint64 timestamp
    );

    /*//////////////////////////////////////////////////////////////
                              CONSTANTS
    //////////////////////////////////////////////////////////////*/

    /// @notice Maximum age, in seconds, of an attestation document for it to
    ///         be considered fresh. AWS Nitro attestation documents include
    ///         an NSM-signed timestamp; rejecting old ones prevents replay.
    uint256 public constant MAX_AGE = 60 minutes;

    /// @notice Tolerated forward drift between the NSM-signed timestamp and
    ///         `block.timestamp`. Anything beyond this is treated as either
    ///         a misconfigured enclave clock or an attempt to bypass
    ///         {MAX_AGE} by dating the document into the future.
    uint256 public constant CLOCK_SKEW_TOLERANCE = 5 minutes;

    /*//////////////////////////////////////////////////////////////
                                STORAGE
    //////////////////////////////////////////////////////////////*/

    /// @notice `keccak256(abi.encode(pcr0, pcr1, pcr2)) => approved`.
    mapping(bytes32 pcrSetHash => bool approved) public approvedPCRSets;

    /*//////////////////////////////////////////////////////////////
                              CONSTRUCTOR
    //////////////////////////////////////////////////////////////*/

    /// @param certManager_ Address of the pre-deployed {CertManager} that
    ///                     caches verified certificates from the AWS Nitro
    ///                     PKI.
    /// @param owner_       Initial owner allowed to manage {approvedPCRSets}.
    /// @param initialPcr0  Array of PCR0 measurements to approve at deploy
    ///                     time. Must be the same length as `initialPcr1`
    ///                     and `initialPcr2`. May be empty.
    /// @param initialPcr1  Array of PCR1 measurements.
    /// @param initialPcr2  Array of PCR2 measurements.
    constructor(
        ICertManager certManager_,
        address owner_,
        bytes32[] memory initialPcr0,
        bytes32[] memory initialPcr1,
        bytes32[] memory initialPcr2
    ) NitroValidator(certManager_) Ownable(owner_) {
        require(
            initialPcr0.length == initialPcr1.length && initialPcr1.length == initialPcr2.length,
            "initial PCR length mismatch"
        );
        for (uint256 i = 0; i < initialPcr0.length; i++) {
            _approvePCRSet(initialPcr0[i], initialPcr1[i], initialPcr2[i]);
        }
    }

    /*//////////////////////////////////////////////////////////////
                           ALLOWLIST MANAGEMENT
    //////////////////////////////////////////////////////////////*/

    /// @notice Adds a PCR triple to the allowlist. Re-approving an
    ///         already-approved triple is a no-op (no event emitted).
    /// @dev Only callable by the owner.
    function approvePCRSet(bytes32 pcr0, bytes32 pcr1, bytes32 pcr2) external onlyOwner {
        _approvePCRSet(pcr0, pcr1, pcr2);
    }

    /// @notice Removes a PCR triple from the allowlist. Future attestations
    ///         for this image will be rejected with {PCRSetNotApproved}.
    ///         Already-registered keys remain in
    ///         {NitroEnclaveKeyRegistry} until separately revoked there.
    /// @dev Only callable by the owner. Revoking an unknown triple is a no-op.
    function revokePCRSet(bytes32 pcr0, bytes32 pcr1, bytes32 pcr2) external onlyOwner {
        bytes32 h = _pcrSetHash(pcr0, pcr1, pcr2);
        if (approvedPCRSets[h]) {
            approvedPCRSets[h] = false;
            emit PCRSetRevoked(pcr0, pcr1, pcr2);
        }
    }

    /// @notice Returns whether the given PCR triple is currently approved.
    function isPCRSetApproved(bytes32 pcr0, bytes32 pcr1, bytes32 pcr2) external view returns (bool) {
        return approvedPCRSets[_pcrSetHash(pcr0, pcr1, pcr2)];
    }

    /*//////////////////////////////////////////////////////////////
                            VERIFICATION
    //////////////////////////////////////////////////////////////*/

    /// @inheritdoc INitroAttestationVerifier
    /// @dev Side effects: this is not `view` because the underlying
    ///      {NitroValidator.validateAttestation} writes verified cert entries
    ///      to {ICertManager}'s cache when uncached certs are encountered.
    function verifyAttestation(bytes calldata attestationTbs, bytes calldata signature)
        external
        returns (bytes memory publicKey, bytes32 pcr0, bytes32 pcr1, bytes32 pcr2)
    {
        // 1. Full on-chain COSE_Sign1 + cert chain + P-384 verification.
        Ptrs memory ptrs = validateAttestation(attestationTbs, signature);

        // 2. Freshness check.
        _checkFreshness(ptrs.timestamp);

        // 3. Extract PCR0/1/2 digests from the document.
        if (ptrs.pcrs.length < 3) revert InsufficientPcrs(ptrs.pcrs.length);
        pcr0 = _hashPcr(attestationTbs, ptrs.pcrs[0]);
        pcr1 = _hashPcr(attestationTbs, ptrs.pcrs[1]);
        pcr2 = _hashPcr(attestationTbs, ptrs.pcrs[2]);

        // 4. Allowlist check.
        if (!approvedPCRSets[_pcrSetHash(pcr0, pcr1, pcr2)]) {
            revert PCRSetNotApproved(pcr0, pcr1, pcr2);
        }

        // 5. Extract the certified enclave public key. The World Nitro
        //    enclave always emits a 65-byte SEC1-uncompressed
        //    (`0x04 || X || Y`) key.
        if (ptrs.publicKey.isNull() || ptrs.publicKey.length() != 65) {
            revert InvalidEmbeddedPublicKey();
        }
        bytes memory tbsMem = attestationTbs;
        publicKey = tbsMem.slice(ptrs.publicKey.start(), 65);
        if (publicKey[0] != 0x04) revert InvalidEmbeddedPublicKey();

        emit AttestationVerified(keccak256(attestationTbs), publicKey, pcr0, pcr1, pcr2, ptrs.timestamp);
    }

    /*//////////////////////////////////////////////////////////////
                                INTERNAL
    //////////////////////////////////////////////////////////////*/

    function _approvePCRSet(bytes32 pcr0, bytes32 pcr1, bytes32 pcr2) internal {
        bytes32 h = _pcrSetHash(pcr0, pcr1, pcr2);
        if (!approvedPCRSets[h]) {
            approvedPCRSets[h] = true;
            emit PCRSetApproved(pcr0, pcr1, pcr2);
        }
    }

    function _pcrSetHash(bytes32 pcr0, bytes32 pcr1, bytes32 pcr2) internal pure returns (bytes32) {
        return keccak256(abi.encode(pcr0, pcr1, pcr2));
    }

    /// @dev Enforces the freshness window. `timestampMs` is the NSM-signed
    ///      timestamp in milliseconds since the Unix epoch.
    /// @custom:reverts AttestationStale if older than {MAX_AGE}.
    /// @custom:reverts AttestationFromFuture if dated more than
    ///                 {CLOCK_SKEW_TOLERANCE} ahead of `block.timestamp`.
    function _checkFreshness(uint64 timestampMs) internal view {
        uint256 docSeconds = uint256(timestampMs) / 1000;
        if (docSeconds > block.timestamp) {
            uint256 drift = docSeconds - block.timestamp;
            if (drift > CLOCK_SKEW_TOLERANCE) revert AttestationFromFuture(drift);
        } else if (block.timestamp - docSeconds > MAX_AGE) {
            revert AttestationStale(block.timestamp - docSeconds);
        }
    }

    /// @dev Hashes the raw PCR bytes referenced by `pcrPtr` into a 32-byte
    ///      digest. Reads directly from calldata; no memory allocation.
    function _hashPcr(bytes calldata tbs, CborElement pcrPtr) internal pure returns (bytes32 hash) {
        uint256 start = pcrPtr.start();
        uint256 len = pcrPtr.length();
        assembly ("memory-safe") {
            let ptr := mload(0x40)
            calldatacopy(ptr, add(tbs.offset, start), len)
            hash := keccak256(ptr, len)
        }
    }
}
