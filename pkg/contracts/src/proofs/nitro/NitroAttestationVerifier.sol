// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {INitroAttestationVerifier} from "./INitroAttestationVerifier.sol";

/// @title NitroAttestationVerifier
/// @author Worldcoin
/// @notice Trusted-relayer attestation verifier for AWS Nitro Enclaves.
/// @dev A fully on-chain implementation of COSE_Sign1 P-384 signature verification and
///      AWS Nitro X.509 chain validation is currently impractical from a gas
///      perspective. This contract therefore implements a *trusted relayer pattern*:
///      one or more authorized relayers (configured by the owner) verify the
///      attestation document off-chain using the canonical Rust verifier in
///      `proofs/nitro/src/attestation.rs`, then submit the resulting
///      (publicKey, PCR0/1/2) tuple to this contract via {attestVerified}.
///
///      The contract records the attested binding and emits {AttestationVerified} so
///      that downstream contracts (e.g. {NitroEnclaveKeyRegistry}) can rely on it.
///      A subsequent upgrade can swap this contract for one that performs the full
///      cryptographic verification on-chain without affecting the interface.
contract NitroAttestationVerifier is INitroAttestationVerifier, Ownable {
    /*//////////////////////////////////////////////////////////////
                                 ERRORS
    //////////////////////////////////////////////////////////////*/

    /// @notice Thrown when a non-relayer attempts to call a relayer-only function.
    error NotRelayer(address caller);

    /// @notice Thrown when the attestation document is empty.
    error EmptyAttestationDoc();

    /// @notice Thrown when the supplied public key is not exactly 65 bytes (SEC1
    ///         uncompressed secp256k1) or does not begin with the `0x04` prefix.
    error InvalidPublicKey();

    /*//////////////////////////////////////////////////////////////
                                 EVENTS
    //////////////////////////////////////////////////////////////*/

    /// @notice Emitted when a relayer is added or removed.
    event RelayerSet(address indexed relayer, bool authorized);

    /// @notice Emitted when an attestation has been recorded as verified.
    /// @param attestationDigest keccak256 of the raw attestation document, for indexing.
    /// @param publicKey         The 65-byte SEC1-uncompressed enclave public key.
    /// @param pcr0              PCR0 measurement of the attested image.
    /// @param pcr1              PCR1 measurement of the attested image.
    /// @param pcr2              PCR2 measurement of the attested image.
    event AttestationVerified(
        bytes32 indexed attestationDigest,
        bytes publicKey,
        bytes32 pcr0,
        bytes32 pcr1,
        bytes32 pcr2
    );

    /*//////////////////////////////////////////////////////////////
                                STORAGE
    //////////////////////////////////////////////////////////////*/

    /// @notice Authorized off-chain verifier relayers.
    mapping(address relayer => bool authorized) public isRelayer;

    /// @notice keccak256(attestationDoc) => keccak256(abi.encode(publicKey, pcr0,
    ///         pcr1, pcr2)). A non-zero value means the document has been attested
    ///         to bind that particular tuple.
    mapping(bytes32 attestationDigest => bytes32 commitment) public attestations;

    /*//////////////////////////////////////////////////////////////
                              CONSTRUCTOR
    //////////////////////////////////////////////////////////////*/

    /// @param owner_   Owner allowed to manage the relayer set.
    /// @param relayer_ Initial trusted relayer (may be zero to skip).
    constructor(address owner_, address relayer_) Ownable(owner_) {
        if (relayer_ != address(0)) {
            isRelayer[relayer_] = true;
            emit RelayerSet(relayer_, true);
        }
    }

    /*//////////////////////////////////////////////////////////////
                            RELAYER MANAGEMENT
    //////////////////////////////////////////////////////////////*/

    /// @notice Adds or removes an authorized relayer.
    /// @dev Only callable by the owner.
    function setRelayer(address relayer, bool authorized) external onlyOwner {
        isRelayer[relayer] = authorized;
        emit RelayerSet(relayer, authorized);
    }

    /*//////////////////////////////////////////////////////////////
                              ATTESTATION
    //////////////////////////////////////////////////////////////*/

    /// @notice Records an attestation document as off-chain-verified by a trusted
    ///         relayer.
    /// @dev Only callable by an address authorized via {setRelayer}. The full raw
    ///      document bytes are accepted (and hashed) so that off-chain indexers can
    ///      reconstruct the link between the document and its on-chain attestation
    ///      record.
    ///
    /// @param attestationDoc Raw NSM/COSE_Sign1 attestation document bytes.
    /// @param publicKey      65-byte SEC1-uncompressed secp256k1 enclave key bound by
    ///                       the document.
    /// @param pcr0           PCR0 measurement.
    /// @param pcr1           PCR1 measurement.
    /// @param pcr2           PCR2 measurement.
    function attestVerified(
        bytes calldata attestationDoc,
        bytes calldata publicKey,
        bytes32 pcr0,
        bytes32 pcr1,
        bytes32 pcr2
    ) external {
        if (!isRelayer[msg.sender]) revert NotRelayer(msg.sender);
        if (attestationDoc.length == 0) revert EmptyAttestationDoc();
        if (publicKey.length != 65 || publicKey[0] != 0x04) revert InvalidPublicKey();

        bytes32 digest = keccak256(attestationDoc);
        bytes32 commitment = keccak256(abi.encode(publicKey, pcr0, pcr1, pcr2));
        attestations[digest] = commitment;

        emit AttestationVerified(digest, publicKey, pcr0, pcr1, pcr2);
    }

    /*//////////////////////////////////////////////////////////////
                               VIEW HOOKS
    //////////////////////////////////////////////////////////////*/

    /// @inheritdoc INitroAttestationVerifier
    function verifyAttestation(
        bytes calldata attestationDoc,
        bytes calldata publicKey,
        bytes32 pcr0,
        bytes32 pcr1,
        bytes32 pcr2
    ) external view returns (bool ok) {
        bytes32 digest = keccak256(attestationDoc);
        bytes32 expected = keccak256(abi.encode(publicKey, pcr0, pcr1, pcr2));
        return attestations[digest] == expected && expected != bytes32(0);
    }
}
