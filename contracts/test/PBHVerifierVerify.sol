// SPDX-License-Identifier: MIT
pragma solidity ^0.8.21;

import {PBHVerifierTest} from "./PBHVerifierTest.sol";
import {CheckInitialized} from "@world-id-contracts/utils/CheckInitialized.sol";
import {WorldIDImpl} from "@world-id-contracts/abstract/WorldIDImpl.sol";
import {MockWorldIDGroups} from "./MockWorldIDGroups.sol";
import {ByteHasher} from "@helpers/ByteHasher.sol";
import {PBHVerifierImplV1 as PBHVerifierImpl} from "../src/PBHVerifierImplV1.sol";
import {PBHVerifier} from "../src/PBHVerifier.sol";
import "@BokkyPooBahsDateTimeLibrary/BokkyPooBahsDateTimeLibrary.sol";
import "@helpers/PBHExternalNullifier.sol";

/// @title PBHVerifer Verify Tests
/// @notice Contains tests for the pbhVerifier
/// @author Worldcoin
contract PBHVerifierVerify is PBHVerifierTest {
    using ByteHasher for bytes;

    event PBH(
        uint256 indexed root,
        address indexed sender,
        uint256 nonce,
        bytes callData,
        uint256 indexed pbhExternalNullifier,
        uint256 nullifierHash,
        uint256[8] proof
    );

    address internal sender = address(0x123);
    uint256 internal root = 1;
    uint256 internal nonce = 1;
    bytes internal testCallData = hex"deadbeef";
    uint256[8] internal proof = [uint256(0), 0, 0, 0, 0, 0, 0, 0];

    function getValidPBHExternalNullifier() public view returns (uint256) {
        uint8 month = uint8(BokkyPooBahsDateTimeLibrary.getMonth(block.timestamp));
        uint16 year = uint16(BokkyPooBahsDateTimeLibrary.getYear(block.timestamp));
        return PBHExternalNullifier.encode(0, month, year);
    }

    /// @notice Test that a valid proof is verified correctly.
    function testVerifyPbhProofSuccess() public {
        uint256 nullifierHash = 0;
        uint256 pbhExternalNullifier = getValidPBHExternalNullifier();

        // Expect revert when proof verification fails
        MockWorldIDGroups(address(worldID)).setVerifyProofSuccess(false);
        vm.expectRevert("Proof verification failed");
        PBHVerifierImpl(address(pbhVerifier)).verifyPbhProof(
            root, sender, nonce, testCallData, pbhExternalNullifier, nullifierHash, proof
        );

        // Now expect success
        MockWorldIDGroups(address(worldID)).setVerifyProofSuccess(true);
        vm.expectEmit(true, true, true, true);
        emit PBH(root, sender, nonce, testCallData, pbhExternalNullifier, nullifierHash, proof);
        PBHVerifierImpl(address(pbhVerifier)).verifyPbhProof(
            root, sender, nonce, testCallData, pbhExternalNullifier, nullifierHash, proof
        );

        // Make sure the nullifier hash is marked as used
        bool used = PBHVerifierImpl(address(pbhVerifier)).nullifierHashes(nullifierHash);
        assertTrue(used, "Nullifier hash should be marked as used");

        // Now try to use the same nullifier hash again
        vm.expectRevert(PBHVerifierImpl.InvalidNullifier.selector);
        PBHVerifierImpl(address(pbhVerifier)).verifyPbhProof(
            root, sender, nonce, testCallData, pbhExternalNullifier, nullifierHash, proof
        );
    }

    /// @notice Test that setNumPBHPerMonth works as expected
    function testSetNumPBHPerMonth() public {
        MockWorldIDGroups(address(worldID)).setVerifyProofSuccess(true);
        uint8 month = uint8(BokkyPooBahsDateTimeLibrary.getMonth(block.timestamp));
        uint16 year = uint16(BokkyPooBahsDateTimeLibrary.getYear(block.timestamp));

        // Value starts at 30, make sure 30 reverts.
        uint256 pbhExternalNullifier = PBHExternalNullifier.encode(30, month, year);
        uint256 nullifierHash = 0;
        vm.expectRevert(PBHExternalNullifier.InvalidPbhNonce.selector);
        PBHVerifierImpl(address(pbhVerifier)).verifyPbhProof(
            root, sender, nonce, testCallData, pbhExternalNullifier, nullifierHash, proof
        );

        // Increase numPbhPerMonth from non owner, expect revert
        vm.prank(address(123));
        vm.expectRevert("Ownable: caller is not the owner");
        PBHVerifierImpl(address(pbhVerifier)).setNumPbhPerMonth(40);

        // Increase numPbhPerMonth from owner
        vm.prank(thisAddress);
        PBHVerifierImpl(address(pbhVerifier)).setNumPbhPerMonth(40);

        // Try again, it should work
        pbhExternalNullifier = PBHExternalNullifier.encode(30, month, year);
        nullifierHash = 1;
        vm.expectEmit(true, true, true, true);
        emit PBH(root, sender, nonce, testCallData, pbhExternalNullifier, nullifierHash, proof);
        PBHVerifierImpl(address(pbhVerifier)).verifyPbhProof(
            root, sender, nonce, testCallData, pbhExternalNullifier, nullifierHash, proof
        );
    }
}
