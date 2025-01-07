// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @title SafeModuleSignatures
/// @notice Library for determining a variable-threshold signature length.
library SafeModuleSignatures {
    /// @notice Thrown when the length of the signature is less than the minimum required.
    /// @param min The minimum required length.
    /// @param actual The actual length of the signature.
    error InvalidSignatureLength(uint256 min, uint256 actual);

    /// @notice Returns the expected length of the signatures.
    /// @param signatures Signature data.
    /// @param threshold The Signer threshold.
    /// @return expectedLength The expected length of the signatures.
    function _signatureLength(bytes calldata signatures, uint256 threshold)
        internal
        pure
        returns (uint256 expectedLength)
    {
        expectedLength = 0x41 * threshold;
        if (signatures.length < expectedLength) {
            revert InvalidSignatureLength(expectedLength, signatures.length);
        }

        for (uint256 i = 0; i < threshold; i++) {
            uint256 signaturePos = i * 0x41;
            uint8 signatureType = uint8(signatures[signaturePos + 0x40]);

            if (signatureType == 0) {
                uint256 signatureOffset = uint256(bytes32(signatures[signaturePos + 0x20:]));
                uint256 signatureLength = uint256(bytes32(signatures[signatureOffset:]));
                expectedLength += 0x20 + signatureLength;
            }
        }
    }
}
