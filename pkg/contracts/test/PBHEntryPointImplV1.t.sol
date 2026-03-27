// SPDX-License-Identifier: MIT
pragma solidity ^0.8.21;

import {IWorldIDGroups} from "@world-id-contracts/interfaces/IWorldIDGroups.sol";
import {MockWorldIDGroups} from "./mocks/MockWorldIDGroups.sol";
import {CheckInitialized} from "@world-id-contracts/utils/CheckInitialized.sol";
import {WorldIDImpl} from "@world-id-contracts/abstract/WorldIDImpl.sol";
import {ByteHasher} from "@pbh-lib/ByteHasher.sol";
import {IPBHEntryPoint} from "../src/pbh/interfaces/IPBHEntryPoint.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";
import {PBHEntryPointImplV1} from "../src/pbh/PBHEntryPointImplV1.sol";
import {IMulticall3} from "../src/pbh/interfaces/IMulticall3.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {UserOperationLib} from "@account-abstraction/contracts/core/UserOperationLib.sol";
import {PackedUserOperation} from "@account-abstraction/contracts/interfaces/PackedUserOperation.sol";
import {TestSetup} from "./TestSetup.sol";
import {TestUtils} from "./TestUtils.sol";
import {Safe4337Module} from "@4337/Safe4337Module.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import "@account-abstraction/contracts/interfaces/PackedUserOperation.sol";
import "@pbh-lib/PBHExternalNullifier.sol";

/// @title PBHEntryPointImplV1 Tests
/// @notice Contains tests for the PBHEntryPointImplV1 contract
/// @author Worldcoin
contract PBHEntryPointImplV1Test is TestSetup {
    using ByteHasher for bytes;
    using UserOperationLib for PackedUserOperation;

    event PBH(address indexed sender, bytes32 indexed userOpHash, IPBHEntryPoint.PBHPayload payload);
    event NumPbhPerMonthSet(uint16 indexed numPbhPerMonth);
    event WorldIdSet(address indexed worldId);

    function test_verifyPbh(address sender, uint8 pbhNonce) public view {
        vm.assume(pbhNonce < MAX_NUM_PBH_PER_MONTH);

        uint256 extNullifier = TestUtils.getPBHExternalNullifier(pbhNonce);
        IPBHEntryPoint.PBHPayload memory testPayload = TestUtils.mockPBHPayload(0, pbhNonce, extNullifier);
        bytes memory testCallData = hex"c0ffee";

        uint256 signalHash = abi.encodePacked(sender, pbhNonce, testCallData).hashToField();
        pbhEntryPoint.verifyPbh(signalHash, testPayload);
    }

    function test_verifyPbh_RevertIf_InvalidNullifier(address sender, uint8 pbhNonce) public {
        vm.assume(pbhNonce < MAX_NUM_PBH_PER_MONTH);

        uint256 extNullifier = TestUtils.getPBHExternalNullifier(pbhNonce);
        IPBHEntryPoint.PBHPayload memory testPayload = TestUtils.mockPBHPayload(0, pbhNonce, extNullifier);

        vm.prank(BLOCK_BUILDER);
        uint256[] memory nullifierHashes = new uint256[](1);
        nullifierHashes[0] = testPayload.nullifierHash;
        pbhEntryPoint.spendNullifierHashes(nullifierHashes);

        bytes memory testCallData = hex"c0ffee";
        uint256 signalHash = abi.encodePacked(sender, pbhNonce, testCallData).hashToField();
        vm.expectRevert(
            abi.encodeWithSelector(PBHEntryPointImplV1.InvalidNullifier.selector, testPayload.nullifierHash, signalHash)
        );
        pbhEntryPoint.verifyPbh(signalHash, testPayload);
    }

    function test_handleAggregatedOps() public {
        worldIDGroups.setVerifyProofSuccess(true);
        IPBHEntryPoint.PBHPayload memory proof0 = IPBHEntryPoint.PBHPayload({
            root: 1,
            pbhExternalNullifier: TestUtils.getPBHExternalNullifier(0),
            nullifierHash: 0,
            proof: [uint256(0), 0, 0, 0, 0, 0, 0, 0]
        });

        IPBHEntryPoint.PBHPayload memory proof1 = IPBHEntryPoint.PBHPayload({
            root: 2,
            pbhExternalNullifier: TestUtils.getPBHExternalNullifier(1),
            nullifierHash: 1,
            proof: [uint256(0), 0, 0, 0, 0, 0, 0, 0]
        });

        bytes[] memory proofs = new bytes[](2);
        proofs[0] = abi.encode(proof0);
        proofs[1] = abi.encode(proof1);

        PackedUserOperation[] memory uoTestFixture =
            TestUtils.createUOTestData(vm, PBH_NONCE_KEY, address(pbh4337Module), address(safe), proofs, safeOwnerKey);
        bytes memory aggregatedSignature = pbhAggregator.aggregateSignatures(uoTestFixture);

        IEntryPoint.UserOpsPerAggregator[] memory userOpsPerAggregator = new IEntryPoint.UserOpsPerAggregator[](1);
        userOpsPerAggregator[0] = IEntryPoint.UserOpsPerAggregator({
            aggregator: pbhAggregator, userOps: uoTestFixture, signature: aggregatedSignature
        });

        bytes32 userOpHash0 = pbhEntryPoint.getUserOpHash(uoTestFixture[0]);
        vm.expectEmit(true, true, true, true);
        emit PBH(uoTestFixture[0].sender, userOpHash0, proof0);

        bytes32 userOpHash1 = pbhEntryPoint.getUserOpHash(uoTestFixture[1]);
        vm.expectEmit(true, true, true, true);
        emit PBH(uoTestFixture[1].sender, userOpHash1, proof1);

        pbhEntryPoint.handleAggregatedOps(userOpsPerAggregator, payable(address(this)));
    }

    function test_handleAggregatedOps_EIP1271() public {
        // Set Safe Owner to EIP1271 Validator
        safeOwner = mockEIP1271SignatureValidator;
        // Deploy new Safe, SafeModuleSetup, SafeProxyFactory, and Safe4337Module
        deploySafeAndModule(address(pbhAggregator), 1);
        // Deal the Safe Some ETH.
        vm.deal(address(safe), type(uint128).max);
        // Deposit some funds into the Entry Point from the Safe.
        entryPoint.depositTo{value: 10 ether}(address(safe));

        worldIDGroups.setVerifyProofSuccess(true);
        IPBHEntryPoint.PBHPayload memory proof0 = IPBHEntryPoint.PBHPayload({
            root: 1,
            pbhExternalNullifier: TestUtils.getPBHExternalNullifier(0),
            nullifierHash: 0,
            proof: [uint256(0), 0, 0, 0, 0, 0, 0, 0]
        });

        IPBHEntryPoint.PBHPayload memory proof1 = IPBHEntryPoint.PBHPayload({
            root: 2,
            pbhExternalNullifier: TestUtils.getPBHExternalNullifier(1),
            nullifierHash: 1,
            proof: [uint256(0), 0, 0, 0, 0, 0, 0, 0]
        });

        bytes[] memory proofs = new bytes[](2);
        proofs[0] = abi.encode(proof0);
        proofs[1] = abi.encode(proof1);

        PackedUserOperation[] memory uoTestFixture =
            TestUtils.createUOTestData(vm, PBH_NONCE_KEY, address(pbh4337Module), address(safe), proofs, safeOwnerKey);

        uoTestFixture[0].signature =
            TestUtils.encodeSignature(TestUtils.createUserOpEIP1271Signature(safeOwner), proofs[0]);
        uoTestFixture[1].signature =
            TestUtils.encodeSignature(TestUtils.createUserOpEIP1271Signature(safeOwner), proofs[1]);

        bytes memory aggregatedSignature = pbhAggregator.aggregateSignatures(uoTestFixture);

        IEntryPoint.UserOpsPerAggregator[] memory userOpsPerAggregator = new IEntryPoint.UserOpsPerAggregator[](1);
        userOpsPerAggregator[0] = IEntryPoint.UserOpsPerAggregator({
            aggregator: pbhAggregator, userOps: uoTestFixture, signature: aggregatedSignature
        });

        bytes32 userOpHash0 = pbhEntryPoint.getUserOpHash(uoTestFixture[0]);
        vm.expectEmit(true, true, true, true);
        emit PBH(uoTestFixture[0].sender, userOpHash0, proof0);

        bytes32 userOpHash1 = pbhEntryPoint.getUserOpHash(uoTestFixture[1]);
        vm.expectEmit(true, true, true, true);
        emit PBH(uoTestFixture[1].sender, userOpHash1, proof1);

        pbhEntryPoint.handleAggregatedOps(userOpsPerAggregator, payable(address(this)));
    }

    function test_handleAggregatedOps_RevertIf_Reentrancy() public {
        worldIDGroups.setVerifyProofSuccess(true);
        IPBHEntryPoint.PBHPayload memory proof0 = IPBHEntryPoint.PBHPayload({
            root: 1,
            pbhExternalNullifier: TestUtils.getPBHExternalNullifier(0),
            nullifierHash: 0,
            proof: [uint256(0), 0, 0, 0, 0, 0, 0, 0]
        });

        bytes[] memory proofs = new bytes[](1);
        proofs[0] = abi.encode(proof0);

        PackedUserOperation[] memory uoTestFixture =
            TestUtils.createUOTestData(vm, PBH_NONCE_KEY, address(pbh4337Module), address(safe), proofs, safeOwnerKey);
        bytes memory aggregatedSignature = pbhAggregator.aggregateSignatures(uoTestFixture);

        IEntryPoint.UserOpsPerAggregator[] memory userOpsPerAggregator = new IEntryPoint.UserOpsPerAggregator[](1);
        userOpsPerAggregator[0] = IEntryPoint.UserOpsPerAggregator({
            aggregator: pbhAggregator, userOps: uoTestFixture, signature: aggregatedSignature
        });

        bytes memory innerData = abi.encodeWithSelector(
            PBHEntryPointImplV1.handleAggregatedOps.selector, userOpsPerAggregator, payable(address(this))
        );
        bytes memory data = abi.encodeCall(Safe4337Module.executeUserOp, (address(pbhEntryPoint), 0, innerData, 0));
        userOpsPerAggregator[0].userOps[0].callData = data;
        bytes32 operationHash = pbh4337Module.getOperationHash(userOpsPerAggregator[0].userOps[0]);
        // Recreate the signature
        bytes memory signature = TestUtils.createUserOpECDSASignature(vm, operationHash, safeOwnerKey);
        userOpsPerAggregator[0].userOps[0].signature = bytes.concat(signature, abi.encode(proof0));
        pbhEntryPoint.handleAggregatedOps(userOpsPerAggregator, payable(address(this)));
    }

    function test_validateSignaturesCallback_RevertIf_IncorrectHashedOps() public {
        bytes32 hashedOps = 0x0000000000000000000000000000000000000000000000000000000000000001;
        vm.expectRevert(PBHEntryPointImplV1.InvalidHashedOps.selector);
        pbhEntryPoint.validateSignaturesCallback(hashedOps);
    }

    function test_setNumPbhPerMonth(uint16 numPbh) public {
        vm.assume(numPbh > 0);

        vm.prank(OWNER);
        vm.expectEmit(true, true, true, true);
        emit NumPbhPerMonthSet(numPbh);
        pbhEntryPoint.setNumPbhPerMonth(numPbh);
    }

    function test_setNumPbhPerMonth_RevertIf_NotOwner(uint8 numPbh, address addr) public {
        vm.assume(addr != OWNER);
        vm.prank(addr);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, addr));
        pbhEntryPoint.setNumPbhPerMonth(numPbh);
    }

    function test_setNumPbhPerMonth_RevertIf_InvalidNumPbhPerMonth() public {
        vm.prank(OWNER);
        vm.expectRevert(PBHEntryPointImplV1.InvalidNumPbhPerMonth.selector);
        pbhEntryPoint.setNumPbhPerMonth(0);
    }

    function test_setWorldId(address addr) public {
        vm.assume(addr != address(0));

        vm.prank(OWNER);
        vm.expectEmit(true, true, true, true);
        emit WorldIdSet(addr);
        pbhEntryPoint.setWorldId(addr);
    }

    function test_setWorldId_RevertIf_NotOwner(address addr) public {
        vm.assume(addr != OWNER);
        vm.prank(addr);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, addr));
        pbhEntryPoint.setWorldId(addr);
    }

    function test_addBuilder(address addr) public {
        vm.assume(addr != address(0));
        vm.prank(OWNER);
        vm.expectEmit(true, false, false, false);
        emit PBHEntryPointImplV1.BuilderAuthorized(addr);
        pbhEntryPoint.addBuilder(addr);
    }

    function test_addBuilder_RevertIf_NotOwner(address addr) public {
        vm.assume(addr != OWNER);
        vm.prank(addr);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, addr));
        pbhEntryPoint.addBuilder(addr);
    }

    function test_removeBuilder(address addr) public {
        vm.prank(OWNER);
        vm.expectEmit(true, true, true, true);
        emit PBHEntryPointImplV1.BuilderDeauthorized(addr);
        pbhEntryPoint.removeBuilder(addr);
    }

    function test_removeBuilder_RevertIf_NotOwner(address addr) public {
        vm.assume(addr != OWNER);
        vm.prank(addr);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, addr));

        pbhEntryPoint.removeBuilder(addr);
    }

    function test_spendNullifierHashes(uint256[] memory nullifierHashes) public {
        vm.prank(BLOCK_BUILDER);
        vm.expectEmit(true, true, true, true);
        emit PBHEntryPointImplV1.NullifierHashesSpent(BLOCK_BUILDER, nullifierHashes);
        pbhEntryPoint.spendNullifierHashes(nullifierHashes);
        for (uint256 i = 0; i < nullifierHashes.length; i++) {
            assertEq(pbhEntryPoint.nullifierHashes(nullifierHashes[i]), block.number);
        }
    }

    function test_spendNullifierHashes_RevertIf_NotBlockBuilder(address builder) public {
        uint256[] memory nullifierHashes = new uint256[](3);
        nullifierHashes[0] = uint256(0);
        nullifierHashes[1] = uint256(1);
        nullifierHashes[2] = uint256(2);
        vm.assume(builder != BLOCK_BUILDER);
        vm.prank(builder);
        vm.expectRevert(PBHEntryPointImplV1.UnauthorizedBuilder.selector);
        pbhEntryPoint.spendNullifierHashes(nullifierHashes);
        assertEq(pbhEntryPoint.nullifierHashes(nullifierHashes[0]), 0);
        assertEq(pbhEntryPoint.nullifierHashes(nullifierHashes[1]), 0);
        assertEq(pbhEntryPoint.nullifierHashes(nullifierHashes[2]), 0);
    }

    function test_getUserOpHash(PackedUserOperation memory userOp) public {
        bytes32 userOpHash = pbhEntryPoint.getUserOpHash(userOp);
        bytes32 expectedHash = entryPoint.getUserOpHash(userOp);
        assertEq(userOpHash, expectedHash, "UserOp hash does not match expected hash");
    }

    function test_getFirstUnspentNullifierHash_Returns_CorrectIndex() public {
        vm.prank(BLOCK_BUILDER);

        uint256[] memory nullifierHashes = new uint256[](7);
        for (uint256 i = 0; i < 7; i++) {
            nullifierHashes[i] = i;
        }

        // Spend the first 5
        uint256[] memory nullifierHashesToSpend = new uint256[](5);
        for (uint256 i = 0; i < 5; i++) {
            nullifierHashesToSpend[i] = i;
        }
        pbhEntryPoint.spendNullifierHashes(nullifierHashesToSpend);

        // Expect the first the returned index to be 5
        assertEq(pbhEntryPoint.getFirstUnspentNullifierHash(nullifierHashes), 5);
    }

    function test_getFirstUnspentNullifierHash_Returns_Negative_One() public {
        vm.prank(BLOCK_BUILDER);

        uint256[] memory nullifierHashes = new uint256[](7);
        for (uint256 i = 0; i < 7; i++) {
            nullifierHashes[i] = i;
        }

        pbhEntryPoint.spendNullifierHashes(nullifierHashes);

        uint256[] memory firstThreeNullifierHashes = new uint256[](3);
        for (uint256 i = 0; i < 3; i++) {
            firstThreeNullifierHashes[i] = i;
        }
        // Expect the last returned index to be -1
        assertEq(pbhEntryPoint.getFirstUnspentNullifierHash(firstThreeNullifierHashes), -1);
    }

    function test_getUnspentNullifierHashes() public {
        vm.prank(BLOCK_BUILDER);

        uint256[] memory nullifierHashes = new uint256[](7);
        for (uint256 i = 0; i < 7; i++) {
            nullifierHashes[i] = i;
        }

        uint256[] memory threeHashes = new uint256[](3);

        threeHashes[0] = 1;
        threeHashes[1] = 2;
        threeHashes[2] = 4;

        pbhEntryPoint.spendNullifierHashes(threeHashes);

        uint256[] memory unspentHashes = pbhEntryPoint.getUnspentNullifierHashes(nullifierHashes);
        assertEq(unspentHashes.length, 4);
        assertEq(unspentHashes[0], 0);
        assertEq(unspentHashes[1], 3);
        assertEq(unspentHashes[2], 5);
        assertEq(unspentHashes[3], 6);
    }

    receive() external payable {}
}
