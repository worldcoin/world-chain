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
        PackedUserOperation[] memory uoTestFixture = TestUtils.createUOTestData(
            vm,
            PBH_NONCE_KEY,
            address(pbh4337Module),
            address(safe),
            proofs,
            safeOwnerKey
        );

        bytes memory aggregatedSignature = pbhAggregator.aggregateSignatures(
            uoTestFixture
        );
        IPBHEntryPoint.PBHPayload[] memory decodedProofs = abi.decode(
            aggregatedSignature,
            (IPBHEntryPoint.PBHPayload[])
        );
        assertEq(decodedProofs.length, 2, "Decoded proof length should be 2");
        assertEq(decodedProofs[0].root, proof.root, "Root should match");
        assertEq(
            decodedProofs[0].pbhExternalNullifier,
            proof.pbhExternalNullifier,
            "PBH External Nullifier should match"
        );
        assertEq(
            decodedProofs[0].nullifierHash,
            proof.nullifierHash,
            "Nullifier Hash should match"
        );
        assertEq(
            decodedProofs[0].proof[0],
            proof.proof[0],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[1],
            proof.proof[1],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[2],
            proof.proof[2],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[3],
            proof.proof[3],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[4],
            proof.proof[4],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[5],
            proof.proof[5],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[6],
            proof.proof[6],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[7],
            proof.proof[7],
            "Proof should match"
        );
        assertEq(decodedProofs[1].root, proof.root, "Root should match");
        assertEq(
            decodedProofs[1].pbhExternalNullifier,
            proof.pbhExternalNullifier,
            "PBH External Nullifier should match"
        );
        assertEq(
            decodedProofs[1].nullifierHash,
            proof.nullifierHash,
            "Nullifier Hash should match"
        );
        assertEq(
            decodedProofs[1].proof[0],
            proof.proof[0],
            "Proof should match"
        );
        assertEq(
            decodedProofs[1].proof[1],
            proof.proof[1],
            "Proof should match"
        );
        assertEq(
            decodedProofs[1].proof[2],
            proof.proof[2],
            "Proof should match"
        );
        assertEq(
            decodedProofs[1].proof[3],
            proof.proof[3],
            "Proof should match"
        );
        assertEq(
            decodedProofs[1].proof[4],
            proof.proof[4],
            "Proof should match"
        );
        assertEq(
            decodedProofs[1].proof[5],
            proof.proof[5],
            "Proof should match"
        );
        assertEq(
            decodedProofs[1].proof[6],
            proof.proof[6],
            "Proof should match"
        );
        assertEq(
            decodedProofs[1].proof[7],
            proof.proof[7],
            "Proof should match"
        );
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
        PackedUserOperation[] memory uoTestFixture = new PackedUserOperation[](
            1
        );
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

        bytes memory aggregatedSignature = pbhAggregator.aggregateSignatures(
            uoTestFixture
        );
        IPBHEntryPoint.PBHPayload[] memory decodedProofs = abi.decode(
            aggregatedSignature,
            (IPBHEntryPoint.PBHPayload[])
        );
        assertEq(decodedProofs.length, 1, "Decoded proof length should be 1");
        assertEq(decodedProofs[0].root, proof.root, "Root should match");
        assertEq(
            decodedProofs[0].pbhExternalNullifier,
            proof.pbhExternalNullifier,
            "PBH External Nullifier should match"
        );
        assertEq(
            decodedProofs[0].nullifierHash,
            proof.nullifierHash,
            "Nullifier Hash should match"
        );
        assertEq(
            decodedProofs[0].proof[0],
            proof.proof[0],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[1],
            proof.proof[1],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[2],
            proof.proof[2],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[3],
            proof.proof[3],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[4],
            proof.proof[4],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[5],
            proof.proof[5],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[6],
            proof.proof[6],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[7],
            proof.proof[7],
            "Proof should match"
        );
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
        PackedUserOperation[] memory uoTestFixture = TestUtils.createUOTestData(
            vm,
            PBH_NONCE_KEY,
            address(pbh4337Module),
            address(safe),
            proofs,
            safeOwnerKey
        );

        uoTestFixture[0].signature = bytes.concat(
            uoTestFixture[0].signature,
            new bytes(1)
        );
        vm.expectRevert(
            abi.encodeWithSelector(
                SafeModuleSignatures.InvalidSignatureLength.selector,
                429,
                430
            )
        );
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

        PackedUserOperation[] memory uoTestFixture = new PackedUserOperation[](
            2
        );
        uoTestFixture[0] = TestUtils.createMockUserOperation(
            address(safe),
            PBH_NONCE_KEY,
            0
        );
        uoTestFixture[1] = TestUtils.createMockUserOperation(
            address(safe),
            PBH_NONCE_KEY,
            1
        );

        bytes memory smartContractSignature = TestUtils
            .createUserOpEIP1271Signature(mockEIP1271SignatureValidator);
        uoTestFixture[0].signature = TestUtils.encodeSignature(
            smartContractSignature,
            proofs[0]
        );
        uoTestFixture[1].signature = TestUtils.encodeSignature(
            smartContractSignature,
            proofs[1]
        );

        bytes memory aggregatedSignature = pbhAggregator.aggregateSignatures(
            uoTestFixture
        );

        IPBHEntryPoint.PBHPayload[] memory decodedProofs = abi.decode(
            aggregatedSignature,
            (IPBHEntryPoint.PBHPayload[])
        );
        assertEq(decodedProofs.length, 2, "Decoded proof length should be 2");
        assertEq(decodedProofs[0].root, proof.root, "Root should match");
        assertEq(
            decodedProofs[0].pbhExternalNullifier,
            proof.pbhExternalNullifier,
            "PBH External Nullifier should match"
        );
        assertEq(
            decodedProofs[0].nullifierHash,
            proof.nullifierHash,
            "Nullifier Hash should match"
        );
        assertEq(
            decodedProofs[0].proof[0],
            proof.proof[0],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[1],
            proof.proof[1],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[2],
            proof.proof[2],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[3],
            proof.proof[3],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[4],
            proof.proof[4],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[5],
            proof.proof[5],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[6],
            proof.proof[6],
            "Proof should match"
        );
        assertEq(
            decodedProofs[0].proof[7],
            proof.proof[7],
            "Proof should match"
        );
        assertEq(decodedProofs[1].root, proof.root, "Root should match");
        assertEq(
            decodedProofs[1].pbhExternalNullifier,
            proof.pbhExternalNullifier,
            "PBH External Nullifier should match"
        );
        assertEq(
            decodedProofs[1].nullifierHash,
            proof.nullifierHash,
            "Nullifier Hash should match"
        );
        assertEq(
            decodedProofs[1].proof[0],
            proof.proof[0],
            "Proof should match"
        );
        assertEq(
            decodedProofs[1].proof[1],
            proof.proof[1],
            "Proof should match"
        );
        assertEq(
            decodedProofs[1].proof[2],
            proof.proof[2],
            "Proof should match"
        );
        assertEq(
            decodedProofs[1].proof[3],
            proof.proof[3],
            "Proof should match"
        );
        assertEq(
            decodedProofs[1].proof[4],
            proof.proof[4],
            "Proof should match"
        );
        assertEq(
            decodedProofs[1].proof[5],
            proof.proof[5],
            "Proof should match"
        );
        assertEq(
            decodedProofs[1].proof[6],
            proof.proof[6],
            "Proof should match"
        );
        assertEq(
            decodedProofs[1].proof[7],
            proof.proof[7],
            "Proof should match"
        );
    }

    function testFuzz_ValidateUserOpSignature(
        uint256 root,
        uint256 nullifierHash,
        ProofData calldata proofData,
        uint8 signatureThreshold
    ) public {
        vm.assume(signatureThreshold >= 1);
        deployMockSafe(address(pbhAggregator), signatureThreshold);
        uint256 pbhExternalNullifier = TestUtils.getPBHExternalNullifier(0);
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

        bytes memory sigForUserOp = pbhAggregator.validateUserOpSignature(
            userOp
        );
        assertEq(buffer, sigForUserOp, "Signature should match");
    }

    function test_validateUserOpSignature() public {
        PBHSignatureAggregator pbhAggregator = PBHSignatureAggregator(
            0x39911B3242e952d86270857Bc8eFC3FcE8d84abe
        );
        PackedUserOperation memory userOp = PackedUserOperation({
            sender: address(0xf9d4fE21d6a1ae2A7A3DD202C2BC5aeBb0fe1521),
            nonce: 0x3ce00c0c7ddf202961316e7d14ea6d76693d15b42f17fb30000000000000000,
            initCode: new bytes(0),
            callData: hex"7bb3742800000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            accountGasLimits: 0x000000000000000000000000000fffd30000000000000000000000000000c350,
            preVerificationGas: 0x7a464,
            gasFees: 0x0000000000000000000000003b9aca0000000000000000000000000073140b60,
            paymasterAndData: new bytes(0),
            signature: hex"00000000000000000000000014ed103b18319b0695f9b6a7069af82805625ac72b4e3cd8f1f51f21eeaa2f5037f8424e1a78f20c89398b0e631b89c0676898ed69ad85ba07286a421fd41e311f1b8647fd5cec42c113ea092cc73c3815c0e964fb0a1a488fab13c85105e3993900000000000000000000000000000000000000000000000000000007e90200012a1aafa77d7babf445c2e7b09f03a93a4ed5a068c3263965f5c93b66260eb26a197fb7d9601feeea07ab7e35786a00181e856d255fc82d198855939522c8106514083854e0f2d271ed16447f1f267dffb09fe0dc8ddc7e7613eb8beb1db8b0271bf8396560ba321de78dc768eb24031e14137f80b0ce21d107efd4707f4abf4315077b724981eca2d3d208509f485f2f18f341c802f7a3eac38a2faac5ece6cb14043d2f45ff7c7fe63e3092378d9c629a8abc8577e6913c130a7a246e5d41082b75411b96ac9a19d67c007af7ccfd3dd9ca4cb7a3af048a0871b48a1770166024d1c3a67cb33a19f5565081a1cf2baa00fc83e578702c38daeb502f337887ed12940f3f9cbfbbd1747f9e59728ca916e2276828f546f7d491cc344cc659c3a7"
        });

        bytes memory sigForUserOp = pbhAggregator.validateUserOpSignature(
            userOp
        );
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
        this.runExtractProofTest(
            abi.encode(signatures),
            signatureThreshold,
            buffer,
            expected
        );
    }

    function runExtractProofTest(
        bytes calldata encoded,
        uint256 threshold,
        bytes memory signature,
        bytes memory proof
    ) public {
        bytes memory signatures = abi.decode(encoded, (bytes));
        this._runExtractProofTest(signatures, threshold, signature, proof);
    }

    function _runExtractProofTest(
        bytes calldata signatures,
        uint256 threshold,
        bytes memory signature,
        bytes memory proof
    ) public {
        (
            bytes memory userOpSignature,
            bytes memory proofData
        ) = SafeModuleSignatures.extractProof(signatures, threshold);
        assertEq(userOpSignature, signature, "Signature should match");
        assertEq(proofData, proof, "Proof should match");
    }

    receive() external payable {}
}
