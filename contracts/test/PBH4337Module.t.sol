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
import {Mock4337Module} from "./Mock4337Module.sol";
import {Safe4337Module} from "@4337/Safe4337Module.sol";

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

        // module = new PBHSafe4337Module(
        //     // TODO: Find transaction broadcaster in forge test
        //     owner,
        //     PBH_SIGNATURE_AGGREGATOR,
        //     PBH_NONCE_KEY
        // );

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

        console.log("Is module enabled:", safe.isModuleEnabled(address(module)));
    }

    function testValidateSignaturesWithUserOp() public {
        PackedUserOperation memory userOp = PackedUserOperation({
            sender: address(safe),
            nonce: uint256(PBH_NONCE_KEY) << 64, // Keep the nonce key format
            initCode: "", // Empty for already deployed safe
            callData: abi.encodeCall( // Actual call the Safe will make
                    Safe.execTransaction,
                    (
                        address(0xdead), // target address
                        1 ether, // value
                        bytes(""), // data
                        Enum.Operation.Call,
                        0, // safeTxGas
                        0, // baseGas
                        0, // gasPrice
                        address(0), // gasToken
                        payable(address(0)), // refundReceiver
                        bytes("") // signatures - empty for user op
                    )
                ),
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
            signature: "" // Will be filled after signing
        });

        bytes32 operationHash = module.getOperationHash(userOp);

        // // First create the userOp in memory
        // PackedUserOperation memory userOp = PackedUserOperation({
        //     sender: address(safe),
        //     nonce: uint256(PBH_NONCE_KEY) << 64,
        //     initCode: "",
        //     callData: "0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf",
        //     accountGasLimits: bytes32(0),
        //     preVerificationGas: 0,
        //     gasFees: bytes32(0),
        //     paymasterAndData: "",
        //     signature: ""
        // });

        // // Encode it to bytes
        // bytes memory encodedUserOp = abi.encode(userOp);

        // // Make the call using a raw call to handle the calldata requirement
        // (bool success, bytes memory returnData) = address(module).call(
        //     abi.encodeCall(Safe4337Module.getOperationHash, (abi.decode(encodedUserOp, (PackedUserOperation))))
        // );
        // require(success, "getOperationHash failed");

        // bytes32 operationHash = abi.decode(returnData, (bytes32));

        // // Sign the operation hash
        // (uint8 v, bytes32 r, bytes32 s) = vm.sign(ownerKey, operationHash);

        // // Pack the signature with just time bounds and r,s,v
        // bytes memory signature = abi.encodePacked(
        //     uint48(0), // uint48 (6 bytes)
        //     uint48(0), // uint48 (6 bytes)
        //     r,
        //     s,
        //     v // The raw signature components
        // );
        // // Update userOp with signature
        // userOp.signature = signature;

        // // Encode the function call using abi.encodeCall
        // // bytes memory callData = abi.encodeCall(
        // //     Mock4337Module.validateSignaturesExternal,
        // //     (userOp)
        // // );

        // // module.validateSignaturesExternal(userOp);

        // // bytes memory callData = abi.encodeCall(
        // //     // Safe4337Module.getOperationHash,
        // //     Mock4337Module.validateSignaturesExternal2,
        // //     (userOp)
        // // );

        // // // Make the raw call
        // // (bool success2, bytes memory returnData) = address(safe).call(callData);
        // // require(success2, "validateSignaturesExternal call failed");

        // // console.log("Success:", success2);
        // // console.log("Return data length:", returnData.length);
        // // console.logBytes(returnData);

        // // // // Decode the return value
        // // // uint256 validationData = abi.decode(returnData, (uint256));
        // // // console.log("validationData", returnData);

        // // uint256 validationData = uint256(returnData);

        // // // // Extract validation components
        // // address authorizer = address(uint160(returnData));
        // // uint48 validUntil = uint48(validationData >> 160);
        // // uint48 validAfter = uint48(validationData >> 208);

        // // console.log("authorizer", authorizer);
        // // console.log("validUntil", validUntil);
        // // console.log("validAfter", validAfter);

        // // // Verify signature was valid (authorizer should be address(0) for valid non-PBH signature)
        // // assertEq(authorizer, PBH_SIGNATURE_AGGREGATOR, "PBH Aggregator address not returned");
        // // assertEq(validUntil, 0, "ValidUntil should be 0");
        // // assertEq(validAfter, 0, "ValidAfter should be 0");
    }
}
