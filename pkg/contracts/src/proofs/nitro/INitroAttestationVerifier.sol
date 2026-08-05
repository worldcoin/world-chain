// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title INitroAttestationVerifier
/// @author World Contributors
/// @custom:security-contact security@toolsforhumanity.com
interface INitroAttestationVerifier {
    /// @notice Verify an attestation document, ensure its PCR triple is in
    ///         the owner-managed allowlist, and return the certified key.
    ///
    /// @param attestationTbs    The COSE_Sign1 TBS bytes produced by
    ///                         `NitroValidator.decodeAttestationTbs`.
    /// @param signature        The 96-byte (r||s) P-384 signature over the TBS.
    /// @param attestationSigHints Off-chain modular-inverse hints for the
    ///                         P-384 attestation signature. Each hint is
    ///                         re-verified on-chain so a wrong hint can only
    ///                         cause a revert, never a false accept.
    ///                         Pre-compute with `tools/p384_hints.js`.
    /// @return publicKey The 65-byte SEC1-uncompressed secp256k1 enclave key
    ///                   embedded in the document.
    /// @return pcr0      `keccak256(rawPcr0)` extracted from the document.
    /// @return pcr1      `keccak256(rawPcr1)` extracted from the document.
    /// @return pcr2      `keccak256(rawPcr2)` extracted from the document.
    function verifyAttestation(
        bytes calldata attestationTbs,
        bytes calldata signature,
        bytes calldata attestationSigHints
    ) external returns (bytes memory publicKey, bytes32 pcr0, bytes32 pcr1, bytes32 pcr2);
}
