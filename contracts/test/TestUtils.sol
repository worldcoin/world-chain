// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "@forge-std/console.sol";
import "@forge-std/Vm.sol";
import "@BokkyPooBahsDateTimeLibrary/BokkyPooBahsDateTimeLibrary.sol";
import "@account-abstraction/contracts/interfaces/PackedUserOperation.sol";
import "@helpers/PBHExternalNullifier.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";
import {IAggregator} from "@account-abstraction/contracts/interfaces/IAggregator.sol";
import {Mock4337Module} from "./mocks/Mock4337Module.sol";
import {Safe4337Module} from "@4337/Safe4337Module.sol";
import {IPBHEntryPoint} from "../src/interfaces/IPBHEntryPoint.sol";
import {Safe} from "@safe-global/safe-contracts/contracts/Safe.sol";

library TestUtils {
    /// @notice Encodes the ECDSA signature and proof data into a single bytes array.
    function encodeSignature(bytes memory userOpSignature, bytes memory proofData)
        public
        pure
        returns (bytes memory res)
    {
        res = bytes.concat(userOpSignature, proofData);
    }

    /// @notice Create a test data for UserOperations.
    function createUOTestData(
        Vm vm,
        uint256 nonceKey,
        address module,
        address sender,
        bytes[] memory proofs,
        uint256 signingKey
    ) public view returns (PackedUserOperation[] memory) {
        PackedUserOperation[] memory uOps = new PackedUserOperation[](proofs.length);
        for (uint256 i = 0; i < proofs.length; i++) {
            PackedUserOperation memory uo = createMockUserOperation(sender, nonceKey, i);
            bytes32 operationHash = Mock4337Module(module).getOperationHash(uo);
            bytes memory ecdsaSignature = createUserOpECDSASignature(vm, operationHash, signingKey);
            bytes memory signature = encodeSignature(ecdsaSignature, proofs[i]);
            uo.signature = signature;
            uOps[i] = uo;
        }

        return uOps;
    }

    /// @notice Creates a Mock UserOperation w/ an unsigned signature.
    function createMockUserOperation(address sender, uint256 nonceKey, uint256 nonce)
        public
        pure
        returns (PackedUserOperation memory uo)
    {
        bytes memory data = abi.encodeCall(Safe4337Module.executeUserOp, (address(0), 0, new bytes(0), 0));
        uo = PackedUserOperation({
            sender: sender,
            nonce: (nonceKey << 64) + nonce,
            initCode: new bytes(0),
            callData: data,
            accountGasLimits: 0x0000000000000000000000000000ffd300000000000000000000000000000000,
            preVerificationGas: 21000,
            gasFees: 0x0000000000000000000000000000000100000000000000000000000000000001,
            paymasterAndData: new bytes(0),
            signature: abi.encodePacked(uint48(0), uint48(0))
        });
    }

    /// @notice Creates an ECDSA signature from the UserOperation Hash, and signer.
    function createUserOpECDSASignature(Vm vm, bytes32 operationHash, uint256 signingKey)
        public
        pure
        returns (bytes memory)
    {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(signingKey, operationHash);
        bytes memory signature = abi.encodePacked(uint48(0), uint48(0), r, s, v);
        return signature;
    }

    /// @notice Creates a mock EIP-1271 signature for a given signature verifier.
    function createUserOpEIP1271Signature(address signatureVerifier) public pure returns (bytes memory) {
        // Each signature is 0x41 (65) bytes long, where fixed part of a Safe contract signature is encoded as:
        //      {32-bytes signature verifier}{32-bytes dynamic data position}{1-byte signature type}
        // and the dynamic part is encoded as:
        //      {32-bytes signature length}{bytes signature data}
        // Signature type is 0x01 for EIP-1271
        uint8 v = 0x00;
        // Signature verifier is the address of the contract that verifies the signature
        bytes32 r = bytes32(uint256(uint160(signatureVerifier)));
        // Signature offset in Aggregate signature passed to the verifier
        bytes32 s = bytes32(uint256(0x41)); // 0x20 (r) + 0x20 (s) + 0x01 (v) 0x41 Length
        bytes32 signatureLength = bytes32(uint256(0x20));
        bytes32 signatureData = bytes32(uint256(0));
        bytes memory signature = abi.encodePacked(uint48(0), uint48(0), r, s, v, signatureLength, signatureData);
        return signature;
    }

    function mockPBHPayload(uint256 root, uint8 pbhNonce, uint256 nullifierHash)
        public
        view
        returns (IPBHEntryPoint.PBHPayload memory)
    {
        return IPBHEntryPoint.PBHPayload({
            root: root,
            pbhExternalNullifier: getPBHExternalNullifier(pbhNonce),
            nullifierHash: nullifierHash,
            proof: [uint256(0), 0, 0, 0, 0, 0, 0, 0]
        });
    }

    function getPBHExternalNullifier(uint8 pbhNonce) public view returns (uint256) {
        uint8 month = uint8(BokkyPooBahsDateTimeLibrary.getMonth(block.timestamp));
        uint16 year = uint16(BokkyPooBahsDateTimeLibrary.getYear(block.timestamp));
        return PBHExternalNullifier.encode(PBHExternalNullifier.V1, pbhNonce, month, year);
    }
}
