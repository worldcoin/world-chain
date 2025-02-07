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

    // function testValidateUserOpSignature() public {
    //     PBHSignatureAggregator aggregator = PBHSignatureAggregator(address(0x5FC8d32690cc91D4c39d9d3abcBD16989F875707));

    //     Mock4337Module module = Mock4337Module(address(0x0B306BF915C4d645ff596e518fAf3F9669b97016));

    //     // PackedUserOperation { sender: 0x4dfe55bc8eea517efdbb6b2d056135b350b79ca2, nonce: 20717964813246413805891616768, initCode: 0x, , accountGasLimits: 0x0000000000000000000000000000ffd30000000000000000000000000000c350, preVerificationGas: 60232, gasFees: 0x00000000000000000000000000000001000000000000000000000000257353a9, paymasterAndData: 0x, signature: 0x000000000000000000000000012d623c7238bcec3bed31b25602c709fef665172ed98e46051eeba9497cb7ccd00a38523070df9408087e1409babf440435ee00ab4b4f8b060a83b6313b389d2405276ad6d825269eb0b67a2e1589123ded27c8b8eabfa898ff7e878ad61071ad01000c07e80000000000000000000000000000000000000000000000000000002fdc7e1f7d7acf3c83bb6b9e25053ddef53a62ee0e6aa97e847c2cdd1d3b6706116cbdc00dab45dfd3159679b6191e5a2b637ad8c72bcf1586303a4741caf2031083aaa1f4cb099cf2443fac382f70cfa3d4001cd04fa87fdf2a81776c02fc2710cc109032f3ce1295692f44a0fef1fbf30b95a55af7761c77f80e751c865d3f2fb7297f738d4e653de34e36c66f9aa864111ed32a1d8e18644db4e506e354390f3bf2228b7b26252b1a7daba818550f0335a86fec17cfc8f15123b3b120b1090e87bdb5dabfb19155adea3aff4b49d3c759c436cb5bc325f0ce61bee2827d2a2faa9ffd8a34b1fb588a51213714675bcc2d20178e29013ba4bb31a7df8c33090ad21ba6f71a8c1aac4dd0ea0c136bc727319bc9a79d76e1831a97256de2a5e7 }
    //     PackedUserOperation memory uo = PackedUserOperation({
    //         sender: address(0x4DFE55bC8eEa517efdbb6B2d056135b350b79ca2),
    //         nonce: 20717964813246413805891616768,
    //         initCode: new bytes(0),
    //         callData: hex"7bb3742800000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    //         accountGasLimits: bytes32(0x0000000000000000000000000000ffd30000000000000000000000000000c350),
    //         preVerificationGas: 60232,
    //         gasFees: bytes32(0x00000000000000000000000000000001000000000000000000000000257353a9),
    //         paymasterAndData: new bytes(0),
    //         signature: hex"0000000000000000000000001ed1ca97df17d44a66d02c57b380b79335d36b0a6bcf1d8dddb205c4d3c7d30c73a23b528872e63d3447ee2e27eed6ac5030537a58a39ef650785bdbedd2f01a0105276ad6d825269eb0b67a2e1589123ded27c8b8eabfa898ff7e878ad61071ad01000c07e80000000000000000000000000000000000000000000000000000002fdc7e1f7d7acf3c83bb6b9e25053ddef53a62ee0e6aa97e847c2cdd1d3b670622158f9fcfff615444d0aa282e5a301b425c1ad4299198b33f563cfff9c36cfb131dd854a7ac6002bbb2a541b3ae00a235286f4ab8cb2348ff83740b2d45d53c11e7f04eb887f02ceebd64c340c12e1adc8eeed228555f755938f356d388b60501f4df4a6454e2f1e38e060140758a39a9acfd35b437145619f6cf07a3e0c6100ecab907cd89c5cf59ef3d6d9818ba9e0f54dbb109ed9cab5bea4f59d3b416db0b273a3aecefa9ebd27b7f90fe8c9e519237780d3e620ab4fee43d77e9dd325c0b885f2b47315a7c242d8e5e42c7199878eb74bdf4c5fba01f08c0bd31d280a7255d56528425c7d447c88fdc9983ba58ec099a98e059f749899ba2585fbdca3e"
    //     });
    //     uint256 validationData = module.validateSignaturesExternal(uo);
    //     address authorizer = address(uint160(validationData));
    //     uint48 validUntil = uint48(validationData >> 160);
    //     uint48 validAfter = uint48(validationData >> 208);

    //     console.logAddress(authorizer);
    //     console.log(validUntil);
    //     console.log(validAfter);

    //     bytes memory sigForUO = aggregator.validateUserOpSignature(uo);
    //     console.logBytes(sigForUO);
    // }

    // function testSignature() public {
    //     PBHSignatureAggregator aggregator = PBHSignatureAggregator(address(0x5FC8d32690cc91D4c39d9d3abcBD16989F875707));

    //     Mock4337Module module = Mock4337Module(address(0x0B306BF915C4d645ff596e518fAf3F9669b97016));

    //     // PackedUserOperation { sender: 0x4dfe55bc8eea517efdbb6b2d056135b350b79ca2, nonce: 20717964813246413805891616768, initCode: 0x, , accountGasLimits: 0x0000000000000000000000000000ffd30000000000000000000000000000c350, preVerificationGas: 60232, gasFees: 0x00000000000000000000000000000001000000000000000000000000257353a9, paymasterAndData: 0x, signature: 0x000000000000000000000000012d623c7238bcec3bed31b25602c709fef665172ed98e46051eeba9497cb7ccd00a38523070df9408087e1409babf440435ee00ab4b4f8b060a83b6313b389d2405276ad6d825269eb0b67a2e1589123ded27c8b8eabfa898ff7e878ad61071ad01000c07e80000000000000000000000000000000000000000000000000000002fdc7e1f7d7acf3c83bb6b9e25053ddef53a62ee0e6aa97e847c2cdd1d3b6706116cbdc00dab45dfd3159679b6191e5a2b637ad8c72bcf1586303a4741caf2031083aaa1f4cb099cf2443fac382f70cfa3d4001cd04fa87fdf2a81776c02fc2710cc109032f3ce1295692f44a0fef1fbf30b95a55af7761c77f80e751c865d3f2fb7297f738d4e653de34e36c66f9aa864111ed32a1d8e18644db4e506e354390f3bf2228b7b26252b1a7daba818550f0335a86fec17cfc8f15123b3b120b1090e87bdb5dabfb19155adea3aff4b49d3c759c436cb5bc325f0ce61bee2827d2a2faa9ffd8a34b1fb588a51213714675bcc2d20178e29013ba4bb31a7df8c33090ad21ba6f71a8c1aac4dd0ea0c136bc727319bc9a79d76e1831a97256de2a5e7 }
    //     PackedUserOperation memory uo = PackedUserOperation({
    //         sender: address(0x4DFE55bC8eEa517efdbb6B2d056135b350b79ca2),
    //         nonce: 20717964813246413805891616768,
    //         initCode: new bytes(0),
    //         callData: hex"7bb3742800000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    //         accountGasLimits: bytes32(0x0000000000000000000000000000ffd30000000000000000000000000000c350),
    //         preVerificationGas: 60232,
    //         gasFees: bytes32(0x00000000000000000000000000000001000000000000000000000000257353a9),
    //         paymasterAndData: new bytes(0),
    //         signature: abi.encodePacked(uint48(0), uint48(0))
    //     });

    //     bytes32 operationHash = module.getOperationHash(uo);
    //     uint256 signer = 0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a;
    //     bytes memory opSignature = TestUtils.createUserOpECDSASignature(vm, operationHash, signer);
    //     console.logBytes(opSignature);
    // }

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
