// SPDX-License-Identifier: MIT
pragma solidity ^0.8.21;

import {TestSetup} from "./TestSetup.sol";
import {console} from "@forge-std/console.sol";
import {TestUtils} from "./TestUtils.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";
import {IPBHEntryPoint} from "../src/interfaces/IPBHEntryPoint.sol";
import "@account-abstraction/contracts/interfaces/PackedUserOperation.sol";
import "@BokkyPooBahsDateTimeLibrary/BokkyPooBahsDateTimeLibrary.sol";
import "@lib/PBHExternalNullifier.sol";
import {PBHSignatureAggregator} from "../src/PBHSignatureAggregator.sol";
import {SafeModuleSignatures} from "@lib/SafeModuleSignatures.sol";
import {Mock4337Module} from "./mocks/Mock4337Module.sol";

/// @title PBHSignatureAggregator Tests
/// @notice Contains tests for signature aggregation, extraction, and validation.
/// @author Worldcoin
contract PBHSignatureAggregatorTest is TestSetup {
    struct ProofData {
        uint256 p0;
        uint256 p1;
        uint256 p2;
        uint256 p3;
        uint256 p4;
        uint256 p5;
        uint256 p6;
        uint256 p7;
    }

    function testFuzz_AggregateSignatures(
        uint256 root,
        uint256 pbhExternalNullifier,
        uint256 nullifierHash,
        ProofData calldata proofData
    ) public {
        IPBHEntryPoint.PBHPayload memory proof = IPBHEntryPoint.PBHPayload({
            root: root,
            pbhExternalNullifier: pbhExternalNullifier,
            nullifierHash: nullifierHash,
            proof: [
                proofData.p0,
                proofData.p1,
                proofData.p2,
                proofData.p3,
                proofData.p4,
                proofData.p5,
                proofData.p6,
                proofData.p7
            ]
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
        ProofData calldata proofData
    ) public {
        vm.assume(signatureThreshold >= 1);
        deployMockSafe(address(pbhAggregator), signatureThreshold);
        IPBHEntryPoint.PBHPayload memory proof = IPBHEntryPoint.PBHPayload({
            root: root,
            pbhExternalNullifier: pbhExternalNullifier,
            nullifierHash: nullifierHash,
            proof: [
                proofData.p0,
                proofData.p1,
                proofData.p2,
                proofData.p3,
                proofData.p4,
                proofData.p5,
                proofData.p6,
                proofData.p7
            ]
        });

        bytes memory encodedProof = abi.encode(proof);
        PackedUserOperation[] memory uoTestFixture = new PackedUserOperation[](1);
        bytes memory buffer = new bytes(uint256(65) * signatureThreshold + 12);

        // Set the signature type to 0x01 for each signature
        for (uint256 i = 0; i < signatureThreshold; i++) {
            uint256 wordPos = 12 + (i * 0x41);
            buffer[wordPos + 0x40] = bytes1(0x01);
        }

        bytes memory signature = bytes.concat(buffer, encodedProof);

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
        vm.expectRevert(abi.encodeWithSelector(SafeModuleSignatures.InvalidSignatureLength.selector, 429, 430));
        pbhAggregator.aggregateSignatures(uoTestFixture);
    }

    function testFuzz_AggregateSignatures_EIP1271Signature(
        uint256 root,
        uint256 pbhExternalNullifier,
        uint256 nullifierHash,
        ProofData calldata proofData
    ) public {
        IPBHEntryPoint.PBHPayload memory proof = IPBHEntryPoint.PBHPayload({
            root: root,
            pbhExternalNullifier: pbhExternalNullifier,
            nullifierHash: nullifierHash,
            proof: [
                proofData.p0,
                proofData.p1,
                proofData.p2,
                proofData.p3,
                proofData.p4,
                proofData.p5,
                proofData.p6,
                proofData.p7
            ]
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

    // PackedUserOperation { 
    // sender: 0x4dfe55bc8eea517efdbb6b2d056135b350b79ca2, 
    // nonce: 98268500254511791751890014262541266044804674709396859921428445945272626839552, 
    // initCode: 0x, 
    // callData: 0x7bb3742800000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000, 
    // accountGasLimits: 0x000000000000000000000000000fffd30000000000000000000000000000c350, 
    // preVerificationGas: 60232, 
    // gasFees: 0x0000000000000000000000003b9aca0000000000000000000000000053140b60, 
    // paymasterAndData: 0x, 
    // signature: 0x000000000000000000000000fd2e93e8fd5c92c4a4ba02047f18efdffda1fc7b8a155c6a9730434be0bbe6a9489098947008a2e2988f3ae11218874e7a773ed371b1138e07f8970ebcdd12502005276ad6d825269eb0b67a2e1589123ded27c8b8eabfa898ff7e878ad61071ad01000c07e80000000000000000000000000000000000000000000000000000002fdc7e1f7d7acf3c83bb6b9e25053ddef53a62ee0e6aa97e847c2cdd1d3b670626328b2cb62fe27833c589ab8a900c202fb0e019de76ab7e50b6d87098a3cebc2f79c04f0737cf4551bc209457599e0806b6e36ce9725bddf06a7aeb0fd5ed7127a02c48981c90ed1e47f9492d32a73c930eea823fb8ce7a23f73cff3381890c01c94797dfa4fbc9b1b226d65972c80acf1d10a471801c48aeccbd7b325b50b408b20977c6445febc6a1c8a849bdbc64b4497f08b141418164b31bc999b9869f01f138a142aa38e4454d217e8532290f20e211c5578639f0f7a8a381e02f667020bffc27977f2ca0a735ccf8d946b981443d1026a8b70be166b50005e0357789067cd8f68081a941e1068961830f36a73e24cbc2cd1dac1cb4a781475b5a95c3 
    // }
    function test_validateUserOperation() public {
        Mock4337Module mockModule = Mock4337Module(address(0x0B306BF915C4d645ff596e518fAf3F9669b97016));
        PackedUserOperation memory userOp = PackedUserOperation({
            sender: address(0x4DFE55bC8eEa517efdbb6B2d056135b350b79ca2),
            nonce: 98268500254511791751890014262541266044804674709396859921428445945272626839552,
            initCode: new bytes(0),
            callData: hex"7bb3742800000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            accountGasLimits: 0x000000000000000000000000000fffd30000000000000000000000000000c350,
            preVerificationGas: 60232,
            gasFees: 0x0000000000000000000000003b9aca0000000000000000000000000053140b60,
            paymasterAndData: new bytes(0),
            signature: hex"000000000000000000000000fd2e93e8fd5c92c4a4ba02047f18efdffda1fc7b8a155c6a9730434be0bbe6a9489098947008a2e2988f3ae11218874e7a773ed371b1138e07f8970ebcdd12502005276ad6d825269eb0b67a2e1589123ded27c8b8eabfa898ff7e878ad61071ad01000c07e80000000000000000000000000000000000000000000000000000002fdc7e1f7d7acf3c83bb6b9e25053ddef53a62ee0e6aa97e847c2cdd1d3b670626328b2cb62fe27833c589ab8a900c202fb0e019de76ab7e50b6d87098a3cebc2f79c04f0737cf4551bc209457599e0806b6e36ce9725bddf06a7aeb0fd5ed7127a02c48981c90ed1e47f9492d32a73c930eea823fb8ce7a23f73cff3381890c01c94797dfa4fbc9b1b226d65972c80acf1d10a471801c48aeccbd7b325b50b408b20977c6445febc6a1c8a849bdbc64b4497f08b141418164b31bc999b9869f01f138a142aa38e4454d217e8532290f20e211c5578639f0f7a8a381e02f667020bffc27977f2ca0a735ccf8d946b981443d1026a8b70be166b50005e0357789067cd8f68081a941e1068961830f36a73e24cbc2cd1dac1cb4a781475b5a95c3"
        });

        uint256 validationData = mockModule.validateSignaturesExternal(userOp);
        address authorizer = address(uint160(validationData));
        uint48 validUntil = uint48(validationData >> 160);
        uint48 validAfter = uint48(validationData >> 208);

        console.log("authorizer: %s", authorizer);
        console.log("validUntil: %s", validUntil);
        console.log("validAfter: %s", validAfter);
    }

    function test_decodeAggregatedSignature() public {
        PBHPayload[] memory pbhPayloads = abi.decode(opsPerAggregator[i].signature, (PBHPayload[]));
    }

    function testFuzz_ValidateUserOpSignature(
        uint256 root,
        uint256 pbhExternalNullifier,
        uint256 nullifierHash,
        ProofData calldata proofData,
        uint8 signatureThreshold
    ) public {
        vm.assume(signatureThreshold >= 1);
        deployMockSafe(address(pbhAggregator), signatureThreshold);
        IPBHEntryPoint.PBHPayload memory proof = IPBHEntryPoint.PBHPayload({
            root: root,
            pbhExternalNullifier: pbhExternalNullifier,
            nullifierHash: nullifierHash,
            proof: [
                proofData.p0,
                proofData.p1,
                proofData.p2,
                proofData.p3,
                proofData.p4,
                proofData.p5,
                proofData.p6,
                proofData.p7
            ]
        });

        bytes memory expected = abi.encode(proof);
        bytes memory buffer = new bytes(uint256(65) * signatureThreshold + 12);

        // Set the signature type to 0x01 for each signature
        for (uint256 i = 0; i < signatureThreshold; i++) {
            uint256 wordPos = 12 + (i * 0x41);
            buffer[wordPos + 0x40] = bytes1(0x01);
        }

        bytes memory signatures = bytes.concat(buffer, expected);
        PackedUserOperation memory userOp = PackedUserOperation({
            sender: address(mockSafe),
            nonce: 0,
            initCode: new bytes(0),
            callData: new bytes(0),
            accountGasLimits: 0x00000000000000000000000000009fd300000000000000000000000000000000,
            preVerificationGas: 21000,
            gasFees: 0x0000000000000000000000000000000100000000000000000000000000000001,
            paymasterAndData: new bytes(0),
            signature: signatures
        });

        bytes memory sigForUserOp = pbhAggregator.validateUserOpSignature(userOp);
        assertEq(buffer, sigForUserOp, "Signature should match");
    }

    /// @notice SafeModuleSignatures.extractProof Test
    function testFuzz_ExtractProof(
        uint256 root,
        uint256 pbhExternalNullifier,
        uint256 nullifierHash,
        ProofData calldata proofData,
        uint8 signatureThreshold
    ) public {
        vm.assume(signatureThreshold >= 1);
        IPBHEntryPoint.PBHPayload memory proof = IPBHEntryPoint.PBHPayload({
            root: root,
            pbhExternalNullifier: pbhExternalNullifier,
            nullifierHash: nullifierHash,
            proof: [
                proofData.p0,
                proofData.p1,
                proofData.p2,
                proofData.p3,
                proofData.p4,
                proofData.p5,
                proofData.p6,
                proofData.p7
            ]
        });

        bytes memory expected = abi.encode(proof);
        bytes memory buffer = new bytes(uint256(65) * signatureThreshold + 12);

        // Set the signature type to 0x01 for each signature
        for (uint256 i = 0; i < signatureThreshold; i++) {
            uint256 wordPos = 12 + (i * 0x41);
            buffer[wordPos + 0x40] = bytes1(0x01);
        }

        bytes memory signatures = bytes.concat(buffer, expected);
        this.runExtractProofTest(abi.encode(signatures), signatureThreshold, buffer, expected);
    }

    function runExtractProofTest(bytes calldata encoded, uint256 threshold, bytes memory signature, bytes memory proof)
        public
    {
        bytes memory signatures = abi.decode(encoded, (bytes));
        this._runExtractProofTest(signatures, threshold, signature, proof);
    }

    function _runExtractProofTest(
        bytes calldata signatures,
        uint256 threshold,
        bytes memory signature,
        bytes memory proof
    ) public {
        (bytes memory userOpSignature, bytes memory proofData) =
            SafeModuleSignatures.extractProof(signatures, threshold);
        assertEq(userOpSignature, signature, "Signature should match");
        assertEq(proofData, proof, "Proof should match");
    }

    receive() external payable {}
}
