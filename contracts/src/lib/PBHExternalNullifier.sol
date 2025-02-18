// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import "@BokkyPooBahsDateTimeLibrary/BokkyPooBahsDateTimeLibrary.sol";

/// @title PBHExternalNullifier
/// @notice Library for encoding, decoding, and verifying PBH external nullifiers.
///         External nullifiers are used to uniquely identify actions or events
///         within a specific year and month using a nonce.
/// @dev The encoding format is as follows:
///      - Bits:40-255: Empty
///      - Bits 32-39: Year
///      - Bits 16-31: Month
///      - Bits 8-15: Nonce
///      - Bits 0-7: Version
library PBHExternalNullifier {
    /// @notice Thrown when the provided external nullifier month doesn't
    /// match the current month
    error InvalidExternalNullifierMonth();

    /// @notice Thrown when the external nullifier is invalid
    /// @param externalNullifier The external nullifier that is invalid
    /// @param signalHash The signal hash associated with the PBHPayload
    /// @param reason The reason the external nullifier is invalid
    error InvalidExternalNullifier(uint256 externalNullifier, uint256 signalHash, string reason);

    uint8 public constant V1 = 1;

    /// @notice Encodes a PBH external nullifier using the provided year, month, and nonce.
    /// @param version An 8-bit version number (0-255) used to identify the encoding format.
    /// @param pbhNonce An 8-bit nonce value (0-255) used to uniquely identify the nullifier within a month.
    /// @param month An 8-bit 1-indexed value representing the month (1-12).
    /// @param year A 16-bit value representing the year (e.g., 2024).
    /// @return The encoded PBHExternalNullifier.
    function encode(uint8 version, uint8 pbhNonce, uint8 month, uint16 year) internal pure returns (uint256) {
        require(month > 0 && month < 13, InvalidExternalNullifierMonth());
        return (uint256(year) << 24) | (uint256(month) << 16) | (uint256(pbhNonce) << 8) | uint256(version);
    }

    /// @notice Decodes an encoded PBHExternalNullifier into its constituent components.
    /// @param externalNullifier The encoded external nullifier to decode.
    /// @return version The 8-bit version extracted from the external nullifier.
    /// @return pbhNonce The 8-bit nonce extracted from the external nullifier.
    /// @return month The 8-bit month extracted from the external nullifier.
    /// @return year The 16-bit year extracted from the external nullifier.
    function decode(uint256 externalNullifier)
        internal
        pure
        returns (uint8 version, uint8 pbhNonce, uint8 month, uint16 year)
    {
        year = uint16(externalNullifier >> 24);
        month = uint8((externalNullifier >> 16) & 0xFF);
        pbhNonce = uint8((externalNullifier >> 8) & 0xFF);
        version = uint8(externalNullifier & 0xFF);
    }

    /// @notice Verifies the validity of a PBHExternalNullifier by checking its components.
    /// @param externalNullifier The external nullifier to verify.
    /// @param numPbhPerMonth The number of PBH transactions alloted to each World ID per month, 0 indexed.
    ///         For example, if `numPbhPerMonth` is 29, a user can submit 30 PBH txs.
    /// @param signalHash The signal hash associated with the PBHPayload.
    /// @dev This function ensures the external nullifier matches the current year and month,
    ///      and that the nonce does not exceed `numPbhPerMonth`.
    /// @custom:reverts Reverts if the current block timestamp does not match
    /// the provided month/year or if pbhNonce !<  numPbhPerMonth.
    function verify(uint256 externalNullifier, uint8 numPbhPerMonth, uint256 signalHash) internal view {
        require(
            externalNullifier <= type(uint40).max,
            InvalidExternalNullifier(externalNullifier, signalHash, "Leading zeros")
        );
        (uint8 version, uint8 pbhNonce, uint8 month, uint16 year) = PBHExternalNullifier.decode(externalNullifier);
        require(version == V1, InvalidExternalNullifier(externalNullifier, signalHash, "Invalid Version"));
        require(
            year == BokkyPooBahsDateTimeLibrary.getYear(block.timestamp),
            InvalidExternalNullifier(externalNullifier, signalHash, "Invalid Year")
        );
        require(
            month == BokkyPooBahsDateTimeLibrary.getMonth(block.timestamp),
            InvalidExternalNullifier(externalNullifier, signalHash, "Invalid Month")
        );
        require(pbhNonce < numPbhPerMonth, InvalidExternalNullifier(externalNullifier, signalHash, "Invalid PBH Nonce"));
    }
}
