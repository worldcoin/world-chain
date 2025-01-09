// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Vm} from "forge-std/Vm.sol";
import {Test} from "forge-std/Test.sol";
import "forge-std/console.sol";
import {PackedUserOperation} from "@account-abstraction/contracts/interfaces/PackedUserOperation.sol";

import {Safe} from "@safe-global/safe-contracts/contracts/Safe.sol";
import {SafeProxyFactory} from "@safe-global/safe-contracts/contracts/proxies/SafeProxyFactory.sol";
import {SafeProxy} from "@safe-global/safe-contracts/contracts/proxies/SafeProxy.sol";
import {Enum} from "@safe-global/safe-contracts/contracts/common/Enum.sol";
import {SafeModuleSetup} from "@4337/SafeModuleSetup.sol";
import {PBHSafe4337Module} from "../src/PBH4337Module.sol";
import {Mock4337Module} from "./mocks/Mock4337Module.sol";
import {Safe4337Module} from "@4337/Safe4337Module.sol";

/// @title PBH4337Module Tests
/// @notice Contains tests for the PBH4337Module contract
/// @author Worldcoin
contract PBHSafe4337ModuleTest is Test {
    Mock4337Module public module;
    Safe public singleton;
    Safe public safe;
    SafeProxyFactory public factory;
    SafeModuleSetup public moduleSetup;

    address public owner;
    uint256 public ownerKey;

    address public constant PBH_SIGNATURE_AGGREGATOR = address(0x123);
    uint192 public constant PBH_NONCE_KEY = 1123123123;

    function setUp() public {
        // Create single EOA owner
        ownerKey = 0x1;
        owner = vm.addr(ownerKey);

        module = new Mock4337Module(owner, PBH_SIGNATURE_AGGREGATOR, PBH_NONCE_KEY);

        // Deploy SafeModuleSetup
        moduleSetup = new SafeModuleSetup();

        // Deploy Safe singleton and factory
        singleton = new Safe();
        factory = new SafeProxyFactory();

        // Prepare module initialization
        address[] memory modules = new address[](1);
        modules[0] = address(module);

        // Encode the moduleSetup.enableModules call
        bytes memory moduleSetupCall = abi.encodeCall(SafeModuleSetup.enableModules, (modules));

        // Create owners array with single owner
        address[] memory owners = new address[](1);
        owners[0] = owner;

        // Encode initialization data for proxy
        bytes memory initData = abi.encodeCall(
            Safe.setup,
            (
                owners,
                1, // threshold
                address(moduleSetup), // to
                moduleSetupCall, // data
                address(0), // fallbackHandler
                address(0), // paymentToken
                0, // payment
                payable(address(0)) // paymentReceiver
            )
        );

        // Deploy and initialize Safe proxy
        SafeProxy proxy = factory.createProxyWithNonce(
            address(singleton),
            initData,
            0 // salt nonce
        );

        // Cast proxy to Safe for easier interaction
        safe = Safe(payable(address(proxy)));
    }

    function testValidSignature_WithProof() public {
        bytes memory signatureBefore = abi.encodePacked(uint48(0), uint48(0));

        PackedUserOperation memory userOp = PackedUserOperation({
            sender: address(safe),
            nonce: uint256(PBH_NONCE_KEY) << 64, // Keep the nonce key format
            initCode: "", // Empty for already deployed safe
            callData: "",
            accountGasLimits: bytes32(
                abi.encode( // Pack verification and call gas limits
                    uint128(100000), // verification gas limit
                    uint128(300000) // call gas limit
                )
            ),
            preVerificationGas: 21000, // Base cost
            gasFees: bytes32(
                abi.encode( // Pack max priority fee and max fee
                    uint128(1 gwei), // maxPriorityFeePerGas
                    uint128(100 gwei) // maxFeePerGas
                )
            ),
            paymasterAndData: "", // No paymaster
            signature: signatureBefore
        });

        bytes32 operationHash = module.getOperationHash(userOp);

        (uint8 v, bytes32 r, bytes32 s) = vm.sign(ownerKey, operationHash);

        bytes memory signature = abi.encodePacked(
            uint48(0),
            uint48(0),
            r,
            s,
            v // The raw signature components
        );
        bytes memory proofBuffer = new bytes(352);
        userOp.signature = bytes.concat(signature, proofBuffer);

        uint256 validationData = module.validateSignaturesExternal(userOp);

        // // Extract validation components
        address authorizer = address(uint160(validationData));
        uint48 validUntil = uint48(validationData >> 160);
        uint48 validAfter = uint48(validationData >> 208);

        // Verify signature was valid (authorizer should be address(0) for valid non-PBH signature)
        assertEq(authorizer, PBH_SIGNATURE_AGGREGATOR, "PBH Aggregator address not returned");
        assertEq(validUntil, 0, "ValidUntil should be 0");
        assertEq(validAfter, 0, "ValidAfter should be 0");
    }

    function testValidSignature_WithoutProof() public {
        bytes memory signatureBefore = abi.encodePacked(uint48(0), uint48(0));

        PackedUserOperation memory userOp = PackedUserOperation({
            sender: address(safe),
            nonce: uint256(PBH_NONCE_KEY) << 64, // Keep the nonce key format
            initCode: "", // Empty for already deployed safe
            callData: "",
            accountGasLimits: bytes32(
                abi.encode( // Pack verification and call gas limits
                    uint128(100000), // verification gas limit
                    uint128(300000) // call gas limit
                )
            ),
            preVerificationGas: 21000, // Base cost
            gasFees: bytes32(
                abi.encode( // Pack max priority fee and max fee
                    uint128(1 gwei), // maxPriorityFeePerGas
                    uint128(100 gwei) // maxFeePerGas
                )
            ),
            paymasterAndData: "", // No paymaster
            signature: signatureBefore
        });

        bytes32 operationHash = module.getOperationHash(userOp);

        (uint8 v, bytes32 r, bytes32 s) = vm.sign(ownerKey, operationHash);

        bytes memory signature = abi.encodePacked(
            uint48(0),
            uint48(0),
            r,
            s,
            v // The raw signature components
        );

        userOp.signature = signature;

        uint256 validationData = module.validateSignaturesExternal(userOp);

        // // Extract validation components
        address authorizer = address(uint160(validationData));
        uint48 validUntil = uint48(validationData >> 160);
        uint48 validAfter = uint48(validationData >> 208);

        // Verify signature was valid (authorizer should be address(0) for valid non-PBH signature)
        assertEq(authorizer, PBH_SIGNATURE_AGGREGATOR, "PBH Aggregator address not returned");
        assertEq(validUntil, 0, "ValidUntil should be 0");
        assertEq(validAfter, 0, "ValidAfter should be 0");
    }

    function testInvalidSignature() public {
        bytes memory signatureBefore = abi.encodePacked(uint48(0), uint48(0));

        PackedUserOperation memory userOp = PackedUserOperation({
            sender: address(safe),
            nonce: uint256(PBH_NONCE_KEY) << 64, // Keep the nonce key format
            initCode: "", // Empty for already deployed safe
            callData: "",
            accountGasLimits: bytes32(
                abi.encode( // Pack verification and call gas limits
                    uint128(100000), // verification gas limit
                    uint128(300000) // call gas limit
                )
            ),
            preVerificationGas: 21000, // Base cost
            gasFees: bytes32(
                abi.encode( // Pack max priority fee and max fee
                    uint128(1 gwei), // maxPriorityFeePerGas
                    uint128(100 gwei) // maxFeePerGas
                )
            ),
            paymasterAndData: "", // No paymaster
            signature: signatureBefore
        });

        // Create an invalid signature
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(ownerKey, keccak256("invalid"));

        bytes memory signature = abi.encodePacked(
            uint48(0),
            uint48(0),
            r,
            s,
            v // The raw signature components
        );
        userOp.signature = signature;

        uint256 validationData = module.validateSignaturesExternal(userOp);

        // // Extract validation components
        address authorizer = address(uint160(validationData));

        // Verify signature was invalid (authorizer should be 1 for invalid signature)
        assertEq(authorizer, address(1), "PBH Aggregator address not returned");
    }
}
