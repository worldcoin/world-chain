// SPDX-License-Identifier: MIT
pragma solidity ^0.8.21;

import {Setup} from "./Setup.sol";
import {IPBHVerifier} from "../src/interfaces/IPBHVerifier.sol";
import {console} from "@forge-std/console.sol";
import "@account-abstraction/contracts/interfaces/PackedUserOperation.sol";

contract PBHSignatureAggregatorTest is Setup {
    PackedUserOperation[] public uoTestFixture;
    IPBHVerifier.PBHPayload public proof = IPBHVerifier.PBHPayload({
        root: 1,
        pbhExternalNullifier: 1,
        nullifierHash: 0,
        proof: [uint256(0), 0, 0, 0, 0, 0, 0, 0]
    });

    function setUp() public override {
        super.setUp();
        uoTestFixture = createUOTestData();
    }

    function testAggregateSignatures() public {
        bytes memory aggregatedSignature = pbhAggregator.aggregateSignatures(uoTestFixture);
        IPBHVerifier.PBHPayload[] memory decodedProofs = abi.decode(aggregatedSignature, (IPBHVerifier.PBHPayload[]));
        assertEq(decodedProofs.length, 1, "Decoded proof length should be 1");
        assertEq(decodedProofs[0].root, proof.root, "Root should match");
        assertEq(
            decodedProofs[0].pbhExternalNullifier, proof.pbhExternalNullifier, "PBH External Nullifier should match"
        );
        assertEq(decodedProofs[0].nullifierHash, proof.nullifierHash, "Nullifier Hash should match");
    }

    function createUOTestData() public view returns (PackedUserOperation[] memory) {
        PackedUserOperation[] memory uOps = new PackedUserOperation[](1);
        bytes memory proofData = abi.encode(proof);
        bytes memory sigBuffer = new bytes(65);
        bytes memory signature = new bytes(417);
        assembly ("memory-safe") {
            mstore(signature, sigBuffer)
            mstore(add(signature, 32), mload(add(sigBuffer, 32)))
            mstore(add(signature, 64), mload(add(sigBuffer, 64)))
            mstore(add(signature, 65), mload(proofData))
            mstore(add(add(signature, 65), 32), mload(add(proofData, 32)))
            mstore(add(add(signature, 65), 64), mload(add(proofData, 64)))
            mstore(add(add(signature, 65), 96), mload(add(proofData, 96)))
            mstore(add(add(signature, 65), 128), mload(add(proofData, 128)))
            mstore(add(add(signature, 65), 160), mload(add(proofData, 160)))
            mstore(add(add(signature, 65), 192), mload(add(proofData, 192)))
            mstore(add(add(signature, 65), 224), mload(add(proofData, 224)))
            mstore(add(add(signature, 65), 256), mload(add(proofData, 256)))
            mstore(add(add(signature, 65), 288), mload(add(proofData, 288)))
            mstore(add(add(signature, 65), 320), mload(add(proofData, 320)))
        }

        uOps[0] = PackedUserOperation({
            sender: address(safe),
            nonce: 0,
            initCode: abi.encodePacked("0x"),
            callData: abi.encodePacked("0x"),
            accountGasLimits: bytes32("10000"),
            preVerificationGas: 10000,
            gasFees: bytes32(0),
            paymasterAndData: abi.encodePacked("0x"),
            signature: signature
        });

        return uOps;
    }
}
