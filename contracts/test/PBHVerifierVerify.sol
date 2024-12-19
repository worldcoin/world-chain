// SPDX-License-Identifier: MIT
pragma solidity ^0.8.21;

import {IWorldIDGroups} from "@world-id-contracts/interfaces/IWorldIDGroups.sol";
import {MockWorldIDGroups} from "./mocks/MockWorldIDGroups.sol";
import {CheckInitialized} from "@world-id-contracts/utils/CheckInitialized.sol";
import {WorldIDImpl} from "@world-id-contracts/abstract/WorldIDImpl.sol";
import {ByteHasher} from "@helpers/ByteHasher.sol";
import "@BokkyPooBahsDateTimeLibrary/BokkyPooBahsDateTimeLibrary.sol";
import "@helpers/PBHExternalNullifier.sol";
import {PBHVerifier} from "../src/PBHVerifier.sol";
import {IPBHVerifier} from "../src/interfaces/IPBHVerifier.sol";
import {Setup} from "./Setup.sol";

/// @title PBHVerifer Verify Tests
/// @notice Contains tests for the pbhVerifier
/// @author Worldcoin
contract PBHVerifierVerify is Setup {
    using ByteHasher for bytes;

    event PBH(
        address indexed sender,
        uint256 indexed nonce,
        bytes callData,
        IPBHVerifier.PBHPayload payload
    );

    /// @notice Test payload for the PBHVerifier
    IPBHVerifier.PBHPayload public testPayload = IPBHVerifier.PBHPayload({
        root: 1,
        pbhExternalNullifier: getValidPBHExternalNullifier(),
        nullifierHash: 1,
        proof: [uint256(0), 0, 0, 0, 0, 0, 0, 0]
    });

    uint256 internal nonce = 1;
    address internal sender = address(0x123);
    bytes internal testCallData = hex"deadbeef";

    function getValidPBHExternalNullifier() public view returns (uint256) {
        uint8 month = uint8(BokkyPooBahsDateTimeLibrary.getMonth(block.timestamp));
        uint16 year = uint16(BokkyPooBahsDateTimeLibrary.getYear(block.timestamp));
        return PBHExternalNullifier.encode(PBHExternalNullifier.V1, 0, month, year);
    }

    /// @notice Test that a valid proof is verified correctly.
    function testVerifyPbhProofSuccess() public {
        // Expect revert when proof verification fails
        MockWorldIDGroups(address(worldIDGroups)).setVerifyProofSuccess(false);
        vm.expectRevert("Proof verification failed");
        pbhEntryPoint.verifyPbhProof(sender, nonce, testCallData, testPayload);

        // Now expect success
        MockWorldIDGroups(address(worldIDGroups)).setVerifyProofSuccess(true);
        vm.expectEmit(true, true, true, true);
        emit PBH(
            sender,
            nonce,
            testCallData,
            testPayload
        );
        pbhEntryPoint.verifyPbhProof(sender, nonce, testCallData, testPayload);

        // Make sure the nullifier hash is marked as used
        bool used = pbhEntryPoint.nullifierHashes(testPayload.nullifierHash);
        assertTrue(used, "Nullifier hash should be marked as used");

        // Now try to use the same nullifier hash again
        vm.expectRevert(PBHVerifier.InvalidNullifier.selector);
        pbhEntryPoint.verifyPbhProof(sender, nonce, testCallData, testPayload);
    }

    /// @notice Test that setNumPBHPerMonth works as expected
    function testSetNumPBHPerMonth() public {
        MockWorldIDGroups(address(worldIDGroups)).setVerifyProofSuccess(true);
        uint8 month = uint8(BokkyPooBahsDateTimeLibrary.getMonth(block.timestamp));
        uint16 year = uint16(BokkyPooBahsDateTimeLibrary.getYear(block.timestamp));

        // Value starts at 30, make sure 30 reverts.
        testPayload.pbhExternalNullifier = PBHExternalNullifier.encode(PBHExternalNullifier.V1, 30, month, year);
        testPayload.nullifierHash = 0;
        vm.expectRevert(PBHExternalNullifier.InvalidPbhNonce.selector);
        pbhEntryPoint.verifyPbhProof(sender, nonce, testCallData, testPayload);

        // Increase numPbhPerMonth from non owner, expect revert
        vm.prank(address(123));
        vm.expectRevert("Ownable: caller is not the owner");
        pbhEntryPoint.setNumPbhPerMonth(40);

        // Increase numPbhPerMonth from owner
        vm.prank(thisAddress);
        pbhEntryPoint.setNumPbhPerMonth(40);

        // Try again, it should work
        testPayload.pbhExternalNullifier = PBHExternalNullifier.encode(PBHExternalNullifier.V1, 30, month, year);
        testPayload.nullifierHash = 1;
        vm.expectEmit(true, true, true, true);

        emit PBH(
            sender,
            nonce,
            testCallData,
            testPayload
        );
        pbhEntryPoint.verifyPbhProof(sender, nonce, testCallData, testPayload);
    }
}
