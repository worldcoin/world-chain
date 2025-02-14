// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Test.sol";
import "@lib/PBHExternalNullifier.sol";
import "@BokkyPooBahsDateTimeLibrary/BokkyPooBahsDateTimeLibrary.sol";

contract CallDepth1 {
    function encodeRevertCallDepth1(uint8 pbhNonce, uint8 month, uint16 year) public pure {
        PBHExternalNullifier.encode(PBHExternalNullifier.V1, pbhNonce, month, year);
    }

    function verifyRevertCallDepth1(uint256 encoded, uint8 maxPbh, uint256 signalHash) public view {
        PBHExternalNullifier.verify(encoded, maxPbh, signalHash);
    }
}

/// @title PBHExternalNullifier Tests
/// @notice Contains tests for the PBHExternalNullifier library
/// @author Worldcoin
contract PBHExternalNullifierTest is Test {
    function testFuzz_encode(uint8 pbhNonce, uint8 month, uint16 year) public pure {
        vm.assume(month > 0 && month <= 12);
        PBHExternalNullifier.encode(PBHExternalNullifier.V1, pbhNonce, month, year);
    }

    function testFuzz_encode_RevertIf_InvalidMonth(uint8 pbhNonce, uint8 month, uint16 year) public {
        vm.assume(month == 0 || month > 12);
        CallDepth1 callDepth1 = new CallDepth1();
        vm.expectRevert(PBHExternalNullifier.InvalidExternalNullifierMonth.selector);
        callDepth1.encodeRevertCallDepth1(pbhNonce, month, year);
    }

    function testFuzz_decode(uint8 pbhNonce, uint8 month, uint16 year) public {
        vm.assume(month > 0 && month <= 12);
        uint256 encoded = PBHExternalNullifier.encode(PBHExternalNullifier.V1, pbhNonce, month, year);

        (uint8 decodedVersion, uint8 decodedNonce, uint8 decodedMonth, uint16 decodedYear) =
            PBHExternalNullifier.decode(encoded);

        assertEq(decodedVersion, PBHExternalNullifier.V1);
        assertEq(decodedNonce, pbhNonce);
        assertEq(decodedMonth, month);
        assertEq(decodedYear, year);
    }

    function testFuzz_verify(uint8 pbhNonce, uint8 month, uint16 year, uint8 maxPbh) public {
        vm.assume(month > 0 && month <= 12);
        vm.assume(year >= 2023);
        vm.assume(maxPbh > 0 && pbhNonce < maxPbh);

        // Warp to timestamp
        uint256 timestamp = BokkyPooBahsDateTimeLibrary.timestampFromDate(year, month, 1);
        vm.warp(timestamp);

        uint256 encoded = PBHExternalNullifier.encode(PBHExternalNullifier.V1, pbhNonce, month, year);
        PBHExternalNullifier.verify(encoded, maxPbh, 0);
    }

    function testFuzz_verify_RevertIf_InvalidNullifierLeadingZeros(uint256 encoded) public {
        vm.assume(encoded > type(uint40).max);
        CallDepth1 callDepth1 = new CallDepth1();
        vm.expectRevert(
            abi.encodeWithSelector(PBHExternalNullifier.InvalidExternalNullifier.selector, encoded, 0, "Leading zeros")
        );
        callDepth1.verifyRevertCallDepth1(encoded, 30, 0);
    }

    function testFuzz_verify_RevertIf_InvalidExternalNullifierVersion(uint8 pbhVersion) public {
        vm.assume(pbhVersion != PBHExternalNullifier.V1);

        uint8 month = uint8(BokkyPooBahsDateTimeLibrary.getMonth(block.timestamp));
        uint16 year = uint16(BokkyPooBahsDateTimeLibrary.getYear(block.timestamp));

        uint8 pbhNonce = 0;
        uint8 maxPbh = 30;
        uint256 encoded = PBHExternalNullifier.encode(pbhVersion, pbhNonce, month, year);
        CallDepth1 callDepth1 = new CallDepth1();
        vm.expectRevert(
            abi.encodeWithSelector(
                PBHExternalNullifier.InvalidExternalNullifier.selector, encoded, 0, "Invalid Version"
            )
        );
        callDepth1.verifyRevertCallDepth1(encoded, maxPbh, 0);
    }

    function testFuzz_verify_RevertIf_InvalidExternalNullifierYear(uint8 month, uint16 year) public {
        vm.assume(month > 0 && month <= 12);
        vm.assume(year >= 2023 && year < type(uint16).max);

        // Warp to timestamp
        uint256 timestamp = BokkyPooBahsDateTimeLibrary.timestampFromDate(year + 1, month, 1);
        vm.warp(timestamp);

        uint8 pbhNonce = 0;
        uint8 maxPbh = 30;
        uint256 encoded = PBHExternalNullifier.encode(PBHExternalNullifier.V1, pbhNonce, month, year);
        CallDepth1 callDepth1 = new CallDepth1();
        vm.expectRevert(
            abi.encodeWithSelector(PBHExternalNullifier.InvalidExternalNullifier.selector, encoded, 0, "Invalid Year")
        );
        callDepth1.verifyRevertCallDepth1(encoded, maxPbh, 0);
    }

    function testFuzz_verify_RevertIf_InvalidExternalNullifierMonth(uint8 month, uint16 year) public {
        vm.assume(month > 0 && month <= 11);
        vm.assume(year >= 2023 && year < type(uint16).max);

        // Warp to timestamp
        uint256 timestamp = BokkyPooBahsDateTimeLibrary.timestampFromDate(year, month + 1, 1);
        vm.warp(timestamp);

        uint8 pbhNonce = 0;
        uint8 maxPbh = 30;
        uint256 encoded = PBHExternalNullifier.encode(PBHExternalNullifier.V1, pbhNonce, month, year);
        CallDepth1 callDepth1 = new CallDepth1();
        vm.expectRevert(
            abi.encodeWithSelector(PBHExternalNullifier.InvalidExternalNullifier.selector, encoded, 0, "Invalid Month")
        );
        callDepth1.verifyRevertCallDepth1(encoded, maxPbh, 0);
    }

    function testFuzz_verify_RevertIf_InvalidPbhNonce(uint8 pbhNonce, uint8 maxPbh) public {
        vm.assume(maxPbh > 0 && pbhNonce >= maxPbh);

        uint8 month = uint8(BokkyPooBahsDateTimeLibrary.getMonth(block.timestamp));
        uint16 year = uint16(BokkyPooBahsDateTimeLibrary.getYear(block.timestamp));

        uint256 encoded = PBHExternalNullifier.encode(PBHExternalNullifier.V1, pbhNonce, month, year);
        CallDepth1 callDepth1 = new CallDepth1();
        vm.expectRevert(
            abi.encodeWithSelector(
                PBHExternalNullifier.InvalidExternalNullifier.selector, encoded, 0, "Invalid PBH Nonce"
            )
        );
        callDepth1.verifyRevertCallDepth1(encoded, maxPbh, 0);
    }
}
