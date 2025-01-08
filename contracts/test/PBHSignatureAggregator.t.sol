// SPDX-License-Identifier: MIT
pragma solidity ^0.8.21;

import {TestSetup} from "./TestSetup.sol";
import {console} from "@forge-std/console.sol";
import {TestUtils} from "./TestUtils.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";
import {IPBHEntryPoint} from "../src/interfaces/IPBHEntryPoint.sol";
import "@account-abstraction/contracts/interfaces/PackedUserOperation.sol";
import "@BokkyPooBahsDateTimeLibrary/BokkyPooBahsDateTimeLibrary.sol";
import "../src/helpers/PBHExternalNullifier.sol";
import {PBHSignatureAggregator} from "../src/PBHSignatureAggregator.sol";

contract PBHSignatureAggregatorTest is TestSetup {
    function testFuzz_AggregateSignatures(
        uint256 root,
        uint256 pbhExternalNullifier,
        uint256 nullifierHash,
        uint256 p0,
        uint256 p1,
        uint256 p2,
        uint256 p3,
        uint256 p4,
        uint256 p5,
        uint256 p6,
        uint256 p7
    ) public {
        IPBHEntryPoint.PBHPayload memory proof = IPBHEntryPoint.PBHPayload({
            root: root,
            pbhExternalNullifier: pbhExternalNullifier,
            nullifierHash: nullifierHash,
            proof: [p0, p1, p2, p3, p4, p5, p6, p7]
        });

        bytes[] memory proofs = new bytes[](2);
        proofs[0] = abi.encode(proof);
        proofs[1] = abi.encode(proof);
        PackedUserOperation[] memory uoTestFixture =
            TestUtils.createUOTestData(vm, PBH_NONCE_KEY, address(pbh4337Module), address(safe), proofs, safeOwnerKey);

        bytes memory aggregatedSignature = pbhAggregator.aggregateSignatures(uoTestFixture);
        IPBHEntryPoint.PBHPayload[] memory decodedProofs =
            abi.decode(aggregatedSignature, (IPBHEntryPoint.PBHPayload[]));
        assertEq(decodedProofs.length, 2, "Decoded proof length should be 2");
        assertEq(decodedProofs[0].root, proof.root, "Root should match");
        assertEq(
            decodedProofs[0].pbhExternalNullifier, proof.pbhExternalNullifier, "PBH External Nullifier should match"
        );
        assertEq(decodedProofs[0].nullifierHash, proof.nullifierHash, "Nullifier Hash should match");
        assertEq(decodedProofs[0].proof[0], proof.proof[0], "Proof should match");
        assertEq(decodedProofs[0].proof[1], proof.proof[1], "Proof should match");
        assertEq(decodedProofs[0].proof[2], proof.proof[2], "Proof should match");
        assertEq(decodedProofs[0].proof[3], proof.proof[3], "Proof should match");
        assertEq(decodedProofs[0].proof[4], proof.proof[4], "Proof should match");
        assertEq(decodedProofs[0].proof[5], proof.proof[5], "Proof should match");
        assertEq(decodedProofs[0].proof[6], proof.proof[6], "Proof should match");
        assertEq(decodedProofs[0].proof[7], proof.proof[7], "Proof should match");
        assertEq(decodedProofs[1].root, proof.root, "Root should match");
        assertEq(
            decodedProofs[1].pbhExternalNullifier, proof.pbhExternalNullifier, "PBH External Nullifier should match"
        );
        assertEq(decodedProofs[1].nullifierHash, proof.nullifierHash, "Nullifier Hash should match");
        assertEq(decodedProofs[1].proof[0], proof.proof[0], "Proof should match");
        assertEq(decodedProofs[1].proof[1], proof.proof[1], "Proof should match");
        assertEq(decodedProofs[1].proof[2], proof.proof[2], "Proof should match");
        assertEq(decodedProofs[1].proof[3], proof.proof[3], "Proof should match");
        assertEq(decodedProofs[1].proof[4], proof.proof[4], "Proof should match");
        assertEq(decodedProofs[1].proof[5], proof.proof[5], "Proof should match");
        assertEq(decodedProofs[1].proof[6], proof.proof[6], "Proof should match");
        assertEq(decodedProofs[1].proof[7], proof.proof[7], "Proof should match");
    }

    function testFuzz_AggregateSignatures_VariableThreshold(
        uint256 root,
        uint256 pbhExternalNullifier,
        uint256 nullifierHash,
        uint8 signatureThreshold,
        uint256 p0,
        uint256 p1,
        uint256 p2,
        uint256 p3,
        uint256 p4,
        uint256 p5,
        uint256 p6,
        uint256 p7
    ) public {
        vm.assume(signatureThreshold >= 1);
        deployMockSafe(address(pbhAggregator), signatureThreshold);
        IPBHEntryPoint.PBHPayload memory proof = IPBHEntryPoint.PBHPayload({
            root: root,
            pbhExternalNullifier: pbhExternalNullifier,
            nullifierHash: nullifierHash,
            proof: [p0, p1, p2, p3, p4, p5, p6, p7]
        });

        bytes memory proofData = abi.encode(proof);
        PackedUserOperation[] memory uoTestFixture = new PackedUserOperation[](1);
        bytes memory buffer = new bytes(uint256(65) * signatureThreshold + 12);

        // Set the signature type to 0x01 for each signature
        for (uint256 i = 0; i < signatureThreshold; i++) {
            uint256 wordPos = 12 + (i * 0x41);
            buffer[wordPos + 0x40] = bytes1(0x01);
        }

        bytes memory signature = bytes.concat(buffer, proofData);

        uoTestFixture[0] = PackedUserOperation({
            sender: address(mockSafe),
            nonce: 0,
            initCode: new bytes(0),
            callData: new bytes(0),
            accountGasLimits: 0x00000000000000000000000000009fd300000000000000000000000000000000,
            preVerificationGas: 21000,
            gasFees: 0x0000000000000000000000000000000100000000000000000000000000000001,
            paymasterAndData: new bytes(0),
            signature: signature
        });

        bytes memory aggregatedSignature = pbhAggregator.aggregateSignatures(uoTestFixture);
        IPBHEntryPoint.PBHPayload[] memory decodedProofs =
            abi.decode(aggregatedSignature, (IPBHEntryPoint.PBHPayload[]));
        assertEq(decodedProofs.length, 1, "Decoded proof length should be 1");
        assertEq(decodedProofs[0].root, proof.root, "Root should match");
        assertEq(
            decodedProofs[0].pbhExternalNullifier, proof.pbhExternalNullifier, "PBH External Nullifier should match"
        );
        assertEq(decodedProofs[0].nullifierHash, proof.nullifierHash, "Nullifier Hash should match");
        assertEq(decodedProofs[0].proof[0], proof.proof[0], "Proof should match");
        assertEq(decodedProofs[0].proof[1], proof.proof[1], "Proof should match");
        assertEq(decodedProofs[0].proof[2], proof.proof[2], "Proof should match");
        assertEq(decodedProofs[0].proof[3], proof.proof[3], "Proof should match");
        assertEq(decodedProofs[0].proof[4], proof.proof[4], "Proof should match");
        assertEq(decodedProofs[0].proof[5], proof.proof[5], "Proof should match");
        assertEq(decodedProofs[0].proof[6], proof.proof[6], "Proof should match");
        assertEq(decodedProofs[0].proof[7], proof.proof[7], "Proof should match");
    }

    function test_AggregateSignatures_RevertIf_InvalidSignatureLength() public {
        IPBHEntryPoint.PBHPayload memory proof = IPBHEntryPoint.PBHPayload({
            root: 0,
            pbhExternalNullifier: 0,
            nullifierHash: 0,
            proof: [uint256(1), 0, 0, 0, 0, 0, 0, 0]
        });

        bytes[] memory proofs = new bytes[](2);
        proofs[0] = abi.encode(proof);
        proofs[1] = abi.encode(proof);
        PackedUserOperation[] memory uoTestFixture =
            TestUtils.createUOTestData(vm, PBH_NONCE_KEY, address(pbh4337Module), address(safe), proofs, safeOwnerKey);

        uoTestFixture[0].signature = bytes.concat(uoTestFixture[0].signature, new bytes(1));
        vm.expectRevert(abi.encodeWithSelector(PBHSignatureAggregator.InvalidSignatureLength.selector, 429, 430));
        pbhAggregator.aggregateSignatures(uoTestFixture);
    }

    function testFuzz_AggregateSignatures_EIP1271Signature(
        uint256 root,
        uint256 pbhExternalNullifier,
        uint256 nullifierHash,
        uint256 p0,
        uint256 p1,
        uint256 p2,
        uint256 p3,
        uint256 p4,
        uint256 p5,
        uint256 p6,
        uint256 p7
    ) public {
        IPBHEntryPoint.PBHPayload memory proof = IPBHEntryPoint.PBHPayload({
            root: root,
            pbhExternalNullifier: pbhExternalNullifier,
            nullifierHash: nullifierHash,
            proof: [p0, p1, p2, p3, p4, p5, p6, p7]
        });

        bytes[] memory proofs = new bytes[](2);
        proofs[0] = abi.encode(proof);
        proofs[1] = abi.encode(proof);

        PackedUserOperation[] memory uoTestFixture = new PackedUserOperation[](2);
        uoTestFixture[0] = TestUtils.createMockUserOperation(address(safe), PBH_NONCE_KEY, 0);
        uoTestFixture[1] = TestUtils.createMockUserOperation(address(safe), PBH_NONCE_KEY, 1);

        bytes memory smartContractSignature = TestUtils.createUserOpEIP1271Signature(mockEIP1271SignatureValidator);
        uoTestFixture[0].signature = TestUtils.encodeSignature(smartContractSignature, proofs[0]);
        uoTestFixture[1].signature = TestUtils.encodeSignature(smartContractSignature, proofs[1]);

        bytes memory aggregatedSignature = pbhAggregator.aggregateSignatures(uoTestFixture);

        IPBHEntryPoint.PBHPayload[] memory decodedProofs =
            abi.decode(aggregatedSignature, (IPBHEntryPoint.PBHPayload[]));
        assertEq(decodedProofs.length, 2, "Decoded proof length should be 2");
        assertEq(decodedProofs[0].root, proof.root, "Root should match");
        assertEq(
            decodedProofs[0].pbhExternalNullifier, proof.pbhExternalNullifier, "PBH External Nullifier should match"
        );
        assertEq(decodedProofs[0].nullifierHash, proof.nullifierHash, "Nullifier Hash should match");
        assertEq(decodedProofs[0].proof[0], proof.proof[0], "Proof should match");
        assertEq(decodedProofs[0].proof[1], proof.proof[1], "Proof should match");
        assertEq(decodedProofs[0].proof[2], proof.proof[2], "Proof should match");
        assertEq(decodedProofs[0].proof[3], proof.proof[3], "Proof should match");
        assertEq(decodedProofs[0].proof[4], proof.proof[4], "Proof should match");
        assertEq(decodedProofs[0].proof[5], proof.proof[5], "Proof should match");
        assertEq(decodedProofs[0].proof[6], proof.proof[6], "Proof should match");
        assertEq(decodedProofs[0].proof[7], proof.proof[7], "Proof should match");
        assertEq(decodedProofs[1].root, proof.root, "Root should match");
        assertEq(
            decodedProofs[1].pbhExternalNullifier, proof.pbhExternalNullifier, "PBH External Nullifier should match"
        );
        assertEq(decodedProofs[1].nullifierHash, proof.nullifierHash, "Nullifier Hash should match");
        assertEq(decodedProofs[1].proof[0], proof.proof[0], "Proof should match");
        assertEq(decodedProofs[1].proof[1], proof.proof[1], "Proof should match");
        assertEq(decodedProofs[1].proof[2], proof.proof[2], "Proof should match");
        assertEq(decodedProofs[1].proof[3], proof.proof[3], "Proof should match");
        assertEq(decodedProofs[1].proof[4], proof.proof[4], "Proof should match");
        assertEq(decodedProofs[1].proof[5], proof.proof[5], "Proof should match");
        assertEq(decodedProofs[1].proof[6], proof.proof[6], "Proof should match");
        assertEq(decodedProofs[1].proof[7], proof.proof[7], "Proof should match");
    }

    receive() external payable {}
}
