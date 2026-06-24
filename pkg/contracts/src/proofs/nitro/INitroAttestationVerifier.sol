// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title INitroAttestationVerifier
/// @author Worldcoin
/// @notice Verifies AWS Nitro NSM / COSE_Sign1 attestation documents and exposes the
///         enclave-certified public key plus the PCR measurements that bind it to a
///         specific enclave image.
/// @dev Full on-chain verification of the COSE_Sign1 P-384 signature and the AWS Nitro
///      certificate chain is intentionally out of scope for the initial deployment — the
///      verification logic is non-trivial and gas-prohibitive. Instead, a trusted
///      off-chain relayer (set as the contract owner) is allowed to attest that an
///      attestation document has been verified off-chain. Future upgrades may replace
///      this with a fully on-chain verifier; the interface is designed so that the
///      switch is transparent to callers.
interface INitroAttestationVerifier {
    /// @notice Verifies an attestation document and returns whether it is valid.
    ///
    /// @param attestationDoc Raw NSM/COSE_Sign1 attestation document bytes.
    /// @param publicKey      The 65-byte uncompressed secp256k1 enclave key claimed
    ///                       by the attestation document.
    /// @param pcr0           Expected PCR0 measurement.
    /// @param pcr1           Expected PCR1 measurement.
    /// @param pcr2           Expected PCR2 measurement.
    ///
    /// @return ok `true` if the document is valid and binds `publicKey` to the
    ///            supplied PCRs.
    function verifyAttestation(
        bytes calldata attestationDoc,
        bytes calldata publicKey,
        bytes32 pcr0,
        bytes32 pcr1,
        bytes32 pcr2
    ) external view returns (bool ok);
}
