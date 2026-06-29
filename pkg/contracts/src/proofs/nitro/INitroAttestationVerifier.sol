// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title INitroAttestationVerifier
/// @author Worldcoin
/// @notice Interface for fully on-chain AWS Nitro Enclaves attestation document
///         verification with an owner-managed PCR allowlist. An implementation
///         MUST:
///           1. parse the COSE_Sign1 to-be-signed (TBS) bytes;
///           2. verify the entire X.509 cert bundle up to the pinned AWS Nitro
///              root CA;
///           3. verify the P-384 signature over the TBS;
///           4. ensure the document is fresh (not stale);
///           5. ensure the PCR0/1/2 values embedded in the doc correspond to
///              an enclave image that the owner has explicitly approved;
///           6. return the certified enclave public key (65-byte SEC1
///              uncompressed secp256k1) **and** the PCR triple it was bound to.
interface INitroAttestationVerifier {
    /// @notice Verify an attestation document, ensure its PCR triple is in
    ///         the owner-managed allowlist, and return the certified key.
    ///
    /// @param attestationTbs The COSE_Sign1 TBS bytes produced by
    ///                       `NitroValidator.decodeAttestationTbs`.
    /// @param signature      The 96-byte (r||s) P-384 signature over the TBS.
    /// @return publicKey The 65-byte SEC1-uncompressed secp256k1 enclave key
    ///                   embedded in the document.
    /// @return pcr0      `keccak256(rawPcr0)` extracted from the document.
    /// @return pcr1      `keccak256(rawPcr1)` extracted from the document.
    /// @return pcr2      `keccak256(rawPcr2)` extracted from the document.
    function verifyAttestation(bytes calldata attestationTbs, bytes calldata signature)
        external
        returns (bytes memory publicKey, bytes32 pcr0, bytes32 pcr1, bytes32 pcr2);
}
