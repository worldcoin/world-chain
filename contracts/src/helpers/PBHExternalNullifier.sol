// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "@BokkyPooBahsDateTimeLibrary/BokkyPooBahsDateTimeLibrary.sol";

/// @title PBHExternalNullifier
/// @notice Library for encoding, decoding, and verifying PBH external nullifiers.
///         External nullifiers are used to uniquely identify actions or events
///         within a specific year and month using a nonce.
/// @dev The encoding format is as follows:
///      - Bits 32-255: Empty
///      - Bits 16-31: Year
///      - Bits 8-15: Month
///      - Bits 0-7: Nonce
library PBHExternalNullifier {
    /// @notice Thrown when the provided external nullifier year doesn't
    /// match the current year
    error InvalidExternalNullifierYear();

    /// @notice Thrown when the provided external nullifier month doesn't
    /// match the current month
    error InvalidExternalNullifierMonth();

    /// @notice Thrown when the provided external
    /// nullifier pbhNonce >= numPbhPerMonth
    error InvalidPbhNonce();

    /// @notice Encodes a PBH external nullifier using the provided year, month, and nonce.
    /// @param pbhNonce An 8-bit nonce value (0-255) used to uniquely identify the nullifier within a month.
    /// @param month An 8-bit 1-indexed value representing the month (1-12).
    /// @param year A 16-bit value representing the year (e.g., 2024).
    /// @return The encoded PBHExternalNullifier.
    function encode(uint8 pbhNonce, uint8 month, uint16 year) internal pure returns (uint256) {
        require(month > 0 && month < 13, InvalidExternalNullifierMonth());
        return (uint32(year) << 16) | (uint32(month) << 8) | uint32(pbhNonce);
    }

    /// @notice Decodes an encoded PBHExternalNullifier into its constituent components.
    /// @param externalNullifier The encoded external nullifier to decode.
    /// @return pbhNonce The 8-bit nonce extracted from the external nullifier.
    /// @return month The 8-bit month extracted from the external nullifier.
    /// @return year The 16-bit year extracted from the external nullifier.
    function decode(uint256 externalNullifier) internal pure returns (uint8 pbhNonce, uint8 month, uint16 year) {
        year = uint16(externalNullifier >> 16);
        month = uint8((externalNullifier >> 8) & 0xFF);
        pbhNonce = uint8(externalNullifier & 0xFF);
    }

    /// @notice Verifies the validity of a PBHExternalNullifier by checking its components.
    /// @param externalNullifier The external nullifier to verify.
    /// @param numPbhPerMonth The maximum allowed value for the `pbhNonce` in the nullifier.
    /// @dev This function ensures the external nullifier matches the current year and month,
    ///      and that the nonce does not exceed `numPbhPerMonth`.
    /// @custom:reverts Reverts if the current block timestamp does not match
    /// the provided month/year or if pbhNonce !<  numPbhPerMonth.
    function verify(uint256 externalNullifier, uint8 numPbhPerMonth) public view {
        (uint8 pbhNonce, uint8 month, uint16 year) = PBHExternalNullifier.decode(externalNullifier);
        require(year == BokkyPooBahsDateTimeLibrary.getYear(block.timestamp), InvalidExternalNullifierYear());
        require(month == BokkyPooBahsDateTimeLibrary.getMonth(block.timestamp), InvalidExternalNullifierMonth());
        require(pbhNonce < numPbhPerMonth, InvalidPbhNonce());
    }
}
