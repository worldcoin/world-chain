// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title INitroAttestationVerifier
/// @author Worldcoin
/// @notice Interface for fully on-chain AWS Nitro Enclaves attestation document
///         verification. An implementation MUST:
///           1. parse the COSE_Sign1 to-be-signed (TBS) bytes;
///           2. verify the entire X.509 cert bundle up to the pinned AWS Nitro root CA;
///           3. verify the P-384 signature over the TBS;
///           4. ensure the document is fresh (not stale);
///           5. ensure the PCR0/1/2 values in the doc match the caller's expectations;
///           6. return the certified enclave public key (65-byte SEC1 uncompressed
///              secp256k1).
interface INitroAttestationVerifier {
    /// @notice Verify an attestation document and return its certified secp256k1
    ///         public key.
    ///
    /// @param attestationTbs The COSE_Sign1 TBS bytes produced by
    ///                       `NitroValidator.decodeAttestationTbs`.
    /// @param signature      The 96-byte (r||s) P-384 signature over the TBS.
    /// @param pcr0           Expected `keccak256(rawPcr0)` (raw PCR is 48 bytes).
    /// @param pcr1           Expected `keccak256(rawPcr1)`.
    /// @param pcr2           Expected `keccak256(rawPcr2)`.
    /// @return publicKey The 65-byte SEC1-uncompressed secp256k1 enclave key
    ///                   embedded in the document.
    function verifyAttestation(
        bytes calldata attestationTbs,
        bytes calldata signature,
        bytes32 pcr0,
        bytes32 pcr1,
        bytes32 pcr2
    ) external returns (bytes memory publicKey);
}
