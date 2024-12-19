// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Test.sol";
import "@helpers/PBHExternalNullifier.sol";
import "@BokkyPooBahsDateTimeLibrary/BokkyPooBahsDateTimeLibrary.sol";

contract PBHExternalNullifierLibTest is Test {
    uint8 public constant VALID_PBH_NONCE = 5;
    uint8 public constant VALID_MONTH = 12;
    uint16 public constant VALID_YEAR = 2024;
    uint8 public constant MAX_PBH_PER_MONTH = 10;

    function testEncodeDecodeValidInput() public {
        uint8 pbhNonce = VALID_PBH_NONCE;
        uint8 month = VALID_MONTH;
        uint16 year = VALID_YEAR;

        uint256 encoded = PBHExternalNullifier.encode(pbhNonce, month, year);
        (uint8 decodedNonce, uint8 decodedMonth, uint16 decodedYear) = PBHExternalNullifier.decode(encoded);

        assertEq(decodedNonce, pbhNonce, "Decoded nonce should match the original");
        assertEq(decodedMonth, month, "Decoded month should match the original");
        assertEq(decodedYear, year, "Decoded year should match the original");
    }

    function testEncodeInvalidMonth() public {
        uint8 invalidMonth = 13;

        vm.expectRevert(PBHExternalNullifier.InvalidExternalNullifierMonth.selector);
        PBHExternalNullifier.encode(VALID_PBH_NONCE, invalidMonth, VALID_YEAR);
    }

    function testVerifyValidExternalNullifier() public {
        // Mock the current date to match VALID_YEAR and VALID_MONTH
        uint256 timestamp = BokkyPooBahsDateTimeLibrary.timestampFromDateTime(VALID_YEAR, VALID_MONTH, 1, 0, 0, 0);
        vm.warp(timestamp);

        uint256 encoded = PBHExternalNullifier.encode(VALID_PBH_NONCE, VALID_MONTH, VALID_YEAR);

        PBHExternalNullifier.verify(encoded, MAX_PBH_PER_MONTH);
    }

    function testVerifyInvalidYear() public {
        uint256 currentTimestamp = BokkyPooBahsDateTimeLibrary.timestampFromDateTime(
            2023, // Mock the year to 2023
            VALID_MONTH,
            1,
            0,
            0,
            0
        );
        vm.warp(currentTimestamp);

        uint256 encoded = PBHExternalNullifier.encode(VALID_PBH_NONCE, VALID_MONTH, VALID_YEAR);

        vm.expectRevert(PBHExternalNullifier.InvalidExternalNullifierYear.selector);
        PBHExternalNullifier.verify(encoded, MAX_PBH_PER_MONTH);
    }

    function testVerifyInvalidMonth() public {
        uint256 currentTimestamp = BokkyPooBahsDateTimeLibrary.timestampFromDateTime(
            VALID_YEAR,
            11, // Mock the month to November
            1,
            0,
            0,
            0
        );
        vm.warp(currentTimestamp);

        uint256 encoded = PBHExternalNullifier.encode(VALID_PBH_NONCE, VALID_MONTH, VALID_YEAR);

        vm.expectRevert(PBHExternalNullifier.InvalidExternalNullifierMonth.selector);
        PBHExternalNullifier.verify(encoded, MAX_PBH_PER_MONTH);
    }

    function testVerifyInvalidPbhNonce() public {
        uint256 timestamp = BokkyPooBahsDateTimeLibrary.timestampFromDateTime(VALID_YEAR, VALID_MONTH, 1, 0, 0, 0);
        vm.warp(timestamp);

        uint256 encoded = PBHExternalNullifier.encode(
            MAX_PBH_PER_MONTH + 1, // Invalid nonce exceeding max
            VALID_MONTH,
            VALID_YEAR
        );

        vm.expectRevert(PBHExternalNullifier.InvalidPbhNonce.selector);
        PBHExternalNullifier.verify(encoded, MAX_PBH_PER_MONTH);
    }
}
