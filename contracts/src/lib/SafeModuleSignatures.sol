// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @title SafeModuleSignatures
/// @notice Library for determining a variable-threshold signature length.
library SafeModuleSignatures {
    /// @notice Thrown when the length of the signature is less than the minimum required.
    /// @param min The minimum required length.
    /// @param actual The actual length of the signature.
    error InvalidSignatureLength(uint256 min, uint256 actual);

    /// @notice The length of an ECDSA signature.
    uint256 internal constant ECDSA_SIGNATURE_LENGTH = 65;
    /// @notice The length of the timestamp bytes.
    /// @dev 6 bytes each for validAfter and validUntil.
    uint256 internal constant TIMESTAMP_BYTES = 12;
    /// @notice The length of the encoded proof data.
    uint256 internal constant PROOF_DATA_LENGTH = 352;

    /// @notice Returns the expected length of the signatures.
    /// @param signatures Signature data.
    /// @param threshold The Signer threshold.
    /// @return expectedLength The expected length of the signatures.
    function signatureLength(bytes calldata signatures, uint256 threshold)
        internal
        pure
        returns (uint256 expectedLength)
    {
        expectedLength = ECDSA_SIGNATURE_LENGTH * threshold;
        if (signatures.length < expectedLength) {
            revert InvalidSignatureLength(expectedLength, signatures.length);
        }

        for (uint256 i = 0; i < threshold; ++i) {
            uint256 signaturePos = i * ECDSA_SIGNATURE_LENGTH;
            uint8 signatureType = uint8(signatures[signaturePos + 0x40]);

            if (signatureType == 0) {
                uint256 signatureOffset = uint256(bytes32(signatures[signaturePos + 0x20:]));
                uint256 length = uint256(bytes32(signatures[signatureOffset:]));
                expectedLength += 0x20 + length;
            }
        }
    }

    /// @notice Utility function to extract the encoded proof data from the signature.
    /// @param signatures Signature data.
    /// @param threshold The Signer threshold.
    /// @return userOperationSignature The user operation signature.
    /// @return proofData The encoded proof data.
    function extractProof(bytes calldata signatures, uint256 threshold)
        internal
        pure
        returns (bytes memory userOperationSignature, bytes memory proofData)
    {
        // Ensure we have the minimum amount of bytes:
        // - 12 Bytes (validUntil, validAfter) 65 Bytes (Fixed ECDSA length) + 352 Bytes (Proof Data)
        require(
            signatures.length >= TIMESTAMP_BYTES + ECDSA_SIGNATURE_LENGTH + PROOF_DATA_LENGTH,
            InvalidSignatureLength(TIMESTAMP_BYTES + ECDSA_SIGNATURE_LENGTH + PROOF_DATA_LENGTH, signatures.length)
        );

        uint256 length = TIMESTAMP_BYTES + SafeModuleSignatures.signatureLength(signatures[TIMESTAMP_BYTES:], threshold);

        require(
            signatures.length == length + PROOF_DATA_LENGTH,
            InvalidSignatureLength(length + PROOF_DATA_LENGTH, signatures.length)
        );

        proofData = signatures[length:length + PROOF_DATA_LENGTH];
        userOperationSignature = signatures[0:length];
    }
}
