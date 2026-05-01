// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";

import {WorldIDAccountManager} from "../src/WorldIDAccountManager.sol";
import {WorldIDAccountManagerImplV1} from "../src/WorldIDAccountManagerImplV1.sol";
import {IWorldIDAccountManager} from "../src/interfaces/IWorldIDAccountManager.sol";
import {IWorldIDVerifier} from "../src/interfaces/IWorldIDVerifier.sol";
import {MockWorldIDVerifier} from "./mocks/MockWorldIDVerifier.sol";

/// @title WorldIDAccountManagerImplV1 tests
/// @notice Exercises every state transition, error path, and signal-hash binding rule of the
///         WIP-1001 Solidity stand-in.
contract WorldIDAccountManagerImplV1Test is Test {
    WorldIDAccountManagerImplV1 internal manager;
    WorldIDAccountManagerImplV1 internal impl;
    MockWorldIDVerifier internal verifier;

    address internal constant OWNER = address(0xC0FFEE);
    address internal constant ATTACKER = address(0xBAD);

    bytes16 internal constant WORLD_ID_ACCOUNT_TAG = "WORLD_ID_ACCOUNT";
    uint64 internal constant WORLD_CHAIN_RP_ID = 480;

    event WorldIDAccountCreated(
        address indexed worldIDAccount, uint256 indexed worldIDAccountNullifier, uint256 indexed sessionId
    );
    event SessionKeysAdded(address indexed worldIDAccount, bytes[] keys);
    event SessionKeysRemoved(address indexed worldIDAccount, bytes[] keys);
    event WorldIDVerifierSet(address indexed worldIDVerifier);

    function setUp() public {
        verifier = new MockWorldIDVerifier(true);
        impl = new WorldIDAccountManagerImplV1();
        bytes memory initCall = abi.encodeCall(WorldIDAccountManagerImplV1.initialize, (verifier, OWNER));
        manager = WorldIDAccountManagerImplV1(address(new WorldIDAccountManager(address(impl), initCall)));
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                HELPERS                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    function _accountAddress(uint256 nullifier_) internal pure returns (address) {
        return address(uint160(uint256(keccak256(abi.encodePacked(nullifier_)))));
    }

    function _action(uint256 nonce_) internal pure returns (uint256) {
        return uint256(keccak256(abi.encodePacked(WORLD_ID_ACCOUNT_TAG, nonce_))) >> 8;
    }

    function _signalHashCreate(IWorldIDAccountManager.WorldIDAccountUpdate memory u_) internal pure returns (uint256) {
        return uint256(keccak256(abi.encode(u_))) >> 8;
    }

    function _signalHashUpdate(IWorldIDAccountManager.WorldIDAccountUpdate memory u_, uint256 generation_)
        internal
        pure
        returns (uint256)
    {
        return uint256(keccak256(abi.encode(u_, generation_))) >> 8;
    }

    function _key(uint8 seed_, uint256 len_) internal pure returns (bytes memory k) {
        k = new bytes(len_);
        for (uint256 i = 0; i < len_; ++i) {
            k[i] = bytes1(uint8(seed_ + i));
        }
    }

    function _singleKey(bytes memory k_) internal pure returns (bytes[] memory arr) {
        arr = new bytes[](1);
        arr[0] = k_;
    }

    function _emptyKeys() internal pure returns (bytes[] memory arr) {
        arr = new bytes[](0);
    }

    function _createUpdate(bytes[] memory addKeys_) internal pure returns (IWorldIDAccountManager.WorldIDAccountUpdate memory u) {
        u = IWorldIDAccountManager.WorldIDAccountUpdate({
            operation: IWorldIDAccountManager.Operation.Create,
            addKeys: addKeys_,
            removeKeys: _emptyKeys()
        });
    }

    function _updatePayload(bytes[] memory addKeys_, bytes[] memory removeKeys_)
        internal
        pure
        returns (IWorldIDAccountManager.WorldIDAccountUpdate memory u)
    {
        u = IWorldIDAccountManager.WorldIDAccountUpdate({
            operation: IWorldIDAccountManager.Operation.Update,
            addKeys: addKeys_,
            removeKeys: removeKeys_
        });
    }

    function _proof() internal pure returns (uint256[5] memory p) {
        p[0] = 1;
        p[1] = 2;
        p[2] = 3;
        p[3] = 4;
        p[4] = 5;
    }

    function _sessionNullifier() internal pure returns (uint256[2] memory n) {
        n[0] = 0xAAAA;
        n[1] = 0xBBBB;
    }

    struct CreateArgs {
        uint256 nullifier;
        uint256 nonce;
        uint256 proofNonce;
        uint256 sessionId;
        uint64 issuerSchemaId;
        uint64 expiresAtMin;
        uint256 credentialGenesisIssuedAtMin;
    }

    function _defaultCreateArgs() internal view returns (CreateArgs memory a) {
        a.nullifier = 0x1234567890ABCDEF;
        a.nonce = 7;
        a.proofNonce = 99;
        a.sessionId = 0xDEADBEEF;
        a.issuerSchemaId = 1;
        a.expiresAtMin = uint64(block.timestamp + 1 days);
        a.credentialGenesisIssuedAtMin = 0;
    }

    function _doCreate(CreateArgs memory a_, IWorldIDAccountManager.WorldIDAccountUpdate memory u_)
        internal
        returns (address acct)
    {
        acct = manager.create(
            a_.nullifier,
            a_.nonce,
            a_.proofNonce,
            a_.sessionId,
            a_.issuerSchemaId,
            a_.expiresAtMin,
            a_.credentialGenesisIssuedAtMin,
            _proof(),
            u_
        );
    }

    function _newAccountWithKey(bytes memory firstKey_) internal returns (address acct, CreateArgs memory a) {
        a = _defaultCreateArgs();
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _createUpdate(_singleKey(firstKey_));
        acct = _doCreate(a, u);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                 CREATE                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_create_happyPath_storage_events_andVerifierCall() public {
        CreateArgs memory a = _defaultCreateArgs();
        bytes memory k = _key(0xAA, 33);
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _createUpdate(_singleKey(k));
        address expectedAcct = _accountAddress(a.nullifier);

        vm.expectCall(
            address(verifier),
            abi.encodeCall(
                IWorldIDVerifier.verify,
                (
                    a.nullifier,
                    _action(a.nonce),
                    WORLD_CHAIN_RP_ID,
                    a.proofNonce,
                    _signalHashCreate(u),
                    a.expiresAtMin,
                    a.issuerSchemaId,
                    a.credentialGenesisIssuedAtMin,
                    _proof()
                )
            )
        );

        vm.expectEmit(true, true, true, true, address(manager));
        emit WorldIDAccountCreated(expectedAcct, a.nullifier, a.sessionId);
        vm.expectEmit(true, true, true, true, address(manager));
        emit SessionKeysAdded(expectedAcct, u.addKeys);

        address acct = _doCreate(a, u);
        assertEq(acct, expectedAcct, "derived address mismatch");
        assertEq(manager.worldIDAccountNullifier(acct), a.nullifier, "nullifier persisted");
        assertEq(manager.sessionIdOf(acct), a.sessionId, "sessionId persisted");
        assertEq(manager.generationOf(acct), 0, "generation starts at 0");

        bytes32[] memory hashes = manager.getSessionKeyHashes(acct);
        assertEq(hashes.length, 1);
        assertEq(hashes[0], keccak256(k));
        assertTrue(manager.isAuthorized(acct, k));
    }

    function test_create_action_topByteIsZero() public {
        CreateArgs memory a = _defaultCreateArgs();
        for (uint256 nonce = 0; nonce < 32; ++nonce) {
            assertEq(_action(nonce) >> 248, 0, "top byte must be zero for V2-verifier compat");
        }
        a.nonce = 1;
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _createUpdate(_singleKey(_key(0x01, 33)));
        _doCreate(a, u);
    }

    function test_create_revertIf_wrongOperation() public {
        CreateArgs memory a = _defaultCreateArgs();
        IWorldIDAccountManager.WorldIDAccountUpdate memory u =
            _updatePayload(_singleKey(_key(0x01, 33)), _emptyKeys());
        vm.expectRevert(IWorldIDAccountManager.InvalidOperation.selector);
        _doCreate(a, u);
    }

    function test_create_revertIf_removeKeysProvided() public {
        CreateArgs memory a = _defaultCreateArgs();
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = IWorldIDAccountManager.WorldIDAccountUpdate({
            operation: IWorldIDAccountManager.Operation.Create,
            addKeys: _singleKey(_key(0x01, 33)),
            removeKeys: _singleKey(_key(0x02, 33))
        });
        vm.expectRevert(IWorldIDAccountManager.InvalidOperation.selector);
        _doCreate(a, u);
    }

    function test_create_revertIf_emptyKeySet() public {
        CreateArgs memory a = _defaultCreateArgs();
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _createUpdate(_emptyKeys());
        vm.expectRevert(IWorldIDAccountManager.EmptyKeySet.selector);
        _doCreate(a, u);
    }

    function test_create_revertIf_tooManyKeys() public {
        CreateArgs memory a = _defaultCreateArgs();
        bytes[] memory keys = new bytes[](21);
        for (uint256 i = 0; i < 21; ++i) {
            keys[i] = _key(uint8(i), 33);
        }
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _createUpdate(keys);
        vm.expectRevert(abi.encodeWithSelector(IWorldIDAccountManager.TooManyKeys.selector, uint256(21), uint256(20)));
        _doCreate(a, u);
    }

    function test_create_revertIf_emptyKey() public {
        CreateArgs memory a = _defaultCreateArgs();
        bytes[] memory keys = new bytes[](1);
        keys[0] = "";
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _createUpdate(keys);
        vm.expectRevert(IWorldIDAccountManager.EmptyKey.selector);
        _doCreate(a, u);
    }

    function test_create_revertIf_keyTooLarge() public {
        CreateArgs memory a = _defaultCreateArgs();
        bytes memory big = _key(0x01, 129);
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _createUpdate(_singleKey(big));
        vm.expectRevert(abi.encodeWithSelector(IWorldIDAccountManager.KeyTooLarge.selector, uint256(129), uint256(128)));
        _doCreate(a, u);
    }

    function test_create_acceptsKeyAtExactBound() public {
        CreateArgs memory a = _defaultCreateArgs();
        bytes memory exactlyMax = _key(0x01, 128);
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _createUpdate(_singleKey(exactlyMax));
        _doCreate(a, u);
    }

    function test_create_revertIf_duplicateKeyInPayload() public {
        CreateArgs memory a = _defaultCreateArgs();
        bytes memory k = _key(0xAA, 33);
        bytes[] memory keys = new bytes[](2);
        keys[0] = k;
        keys[1] = k;
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _createUpdate(keys);
        vm.expectRevert(abi.encodeWithSelector(IWorldIDAccountManager.DuplicateSessionKey.selector, keccak256(k)));
        _doCreate(a, u);
    }

    function test_create_revertIf_alreadyExists() public {
        (address acct,) = _newAccountWithKey(_key(0xAA, 33));
        CreateArgs memory a = _defaultCreateArgs();
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _createUpdate(_singleKey(_key(0xBB, 33)));
        vm.expectRevert(IWorldIDAccountManager.WorldIDAccountAlreadyExists.selector);
        _doCreate(a, u);
        // sanity: prior account is intact
        assertTrue(manager.isAuthorized(acct, _key(0xAA, 33)));
    }

    function test_create_revertIf_zeroNullifier() public {
        CreateArgs memory a = _defaultCreateArgs();
        a.nullifier = 0;
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _createUpdate(_singleKey(_key(0x01, 33)));
        vm.expectRevert(IWorldIDAccountManager.ZeroNullifier.selector);
        _doCreate(a, u);
    }

    function test_create_revertIf_verifierRejects() public {
        verifier.setShouldAccept(false);
        CreateArgs memory a = _defaultCreateArgs();
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _createUpdate(_singleKey(_key(0x01, 33)));
        vm.expectRevert(MockWorldIDVerifier.MockVerifierRejected.selector);
        _doCreate(a, u);
        // No state should be persisted on revert.
        address expected = _accountAddress(a.nullifier);
        assertEq(manager.worldIDAccountNullifier(expected), 0);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                 UPDATE                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_update_add_happyPath_andGenerationBumps() public {
        (address acct,) = _newAccountWithKey(_key(0x01, 33));
        bytes memory k2 = _key(0x02, 33);

        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _updatePayload(_singleKey(k2), _emptyKeys());

        vm.expectCall(
            address(verifier),
            abi.encodeCall(
                IWorldIDVerifier.verifySession,
                (
                    WORLD_CHAIN_RP_ID,
                    777,
                    _signalHashUpdate(u, 0),
                    uint64(block.timestamp + 1 days),
                    uint64(1),
                    0,
                    0xDEADBEEF,
                    _sessionNullifier(),
                    _proof()
                )
            )
        );
        vm.expectEmit(true, true, true, true, address(manager));
        emit SessionKeysAdded(acct, u.addKeys);

        manager.update(acct, 777, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), u);

        assertEq(manager.generationOf(acct), 1, "generation bumps on update");
        assertEq(manager.getSessionKeyHashes(acct).length, 2);
        assertTrue(manager.isAuthorized(acct, k2));
    }

    function test_update_add_revertIf_duplicate() public {
        bytes memory k1 = _key(0x01, 33);
        (address acct,) = _newAccountWithKey(k1);
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _updatePayload(_singleKey(k1), _emptyKeys());
        vm.expectRevert(abi.encodeWithSelector(IWorldIDAccountManager.DuplicateSessionKey.selector, keccak256(k1)));
        manager.update(acct, 1, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), u);
    }

    function test_update_add_revertIf_overflowsMax() public {
        // Create with 20 keys (max), then attempt to add one more.
        CreateArgs memory a = _defaultCreateArgs();
        bytes[] memory initialKeys = new bytes[](20);
        for (uint256 i = 0; i < 20; ++i) {
            initialKeys[i] = _key(uint8(i), 33);
        }
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _createUpdate(initialKeys);
        address acct = _doCreate(a, u);

        IWorldIDAccountManager.WorldIDAccountUpdate memory addOne =
            _updatePayload(_singleKey(_key(0xFF, 33)), _emptyKeys());
        vm.expectRevert(abi.encodeWithSelector(IWorldIDAccountManager.TooManyKeys.selector, uint256(21), uint256(20)));
        manager.update(acct, 1, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), addOne);
    }

    function test_update_revertIf_accountDoesNotExist() public {
        IWorldIDAccountManager.WorldIDAccountUpdate memory u =
            _updatePayload(_singleKey(_key(0x01, 33)), _emptyKeys());
        vm.expectRevert(IWorldIDAccountManager.WorldIDAccountDoesNotExist.selector);
        manager.update(address(0xCAFE), 1, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), u);
    }

    function test_update_revertIf_operationCreate() public {
        (address acct,) = _newAccountWithKey(_key(0x01, 33));
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _createUpdate(_singleKey(_key(0x02, 33)));
        vm.expectRevert(IWorldIDAccountManager.InvalidOperation.selector);
        manager.update(acct, 1, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), u);
    }

    function test_update_remove_happyPath_swapAndPop() public {
        // Create with 3 keys, remove the middle, verify the remaining set is intact.
        bytes memory k1 = _key(0x01, 33);
        bytes memory k2 = _key(0x02, 33);
        bytes memory k3 = _key(0x03, 33);

        CreateArgs memory a = _defaultCreateArgs();
        bytes[] memory initialKeys = new bytes[](3);
        initialKeys[0] = k1;
        initialKeys[1] = k2;
        initialKeys[2] = k3;
        IWorldIDAccountManager.WorldIDAccountUpdate memory createPayload = _createUpdate(initialKeys);
        address acct = _doCreate(a, createPayload);

        IWorldIDAccountManager.WorldIDAccountUpdate memory rm = _updatePayload(_emptyKeys(), _singleKey(k2));
        vm.expectEmit(true, true, true, true, address(manager));
        emit SessionKeysRemoved(acct, rm.removeKeys);
        manager.update(acct, 1, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), rm);

        bytes32[] memory hashes = manager.getSessionKeyHashes(acct);
        assertEq(hashes.length, 2);
        assertTrue(manager.isAuthorized(acct, k1));
        assertFalse(manager.isAuthorized(acct, k2));
        assertTrue(manager.isAuthorized(acct, k3));
        // index for swapped-in element is correct (1-indexed)
        assertEq(manager.keyHashIndex(acct, keccak256(k3)), 2, "k3 should now sit at index 1 (1-indexed: 2)");
    }

    function test_update_addAndRemove_happyPath_removeFirstThenAdd() public {
        bytes memory k1 = _key(0x01, 33);
        bytes memory k2 = _key(0x02, 33);
        bytes memory k3 = _key(0x03, 33);
        bytes memory k4 = _key(0x04, 33);

        CreateArgs memory a = _defaultCreateArgs();
        bytes[] memory initialKeys = new bytes[](3);
        initialKeys[0] = k1;
        initialKeys[1] = k2;
        initialKeys[2] = k3;
        address acct = _doCreate(a, _createUpdate(initialKeys));

        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _updatePayload(_singleKey(k4), _singleKey(k2));

        vm.expectEmit(true, true, true, true, address(manager));
        emit SessionKeysRemoved(acct, u.removeKeys);
        vm.expectEmit(true, true, true, true, address(manager));
        emit SessionKeysAdded(acct, u.addKeys);

        manager.update(acct, 1, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), u);

        bytes32[] memory hashes = manager.getSessionKeyHashes(acct);
        assertEq(hashes.length, 3);
        assertTrue(manager.isAuthorized(acct, k1));
        assertFalse(manager.isAuthorized(acct, k2));
        assertTrue(manager.isAuthorized(acct, k3));
        assertTrue(manager.isAuthorized(acct, k4));
    }

    function test_update_revertIf_addAndRemoveSameExistingKey() public {
        bytes memory k1 = _key(0x01, 33);
        (address acct,) = _newAccountWithKey(k1);

        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _updatePayload(_singleKey(k1), _singleKey(k1));

        vm.expectRevert(abi.encodeWithSelector(IWorldIDAccountManager.OverlappingUpdateKey.selector, keccak256(k1)));
        manager.update(acct, 1, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), u);
    }

    function test_update_revertIf_addAndRemoveSameMissingKey() public {
        bytes memory k1 = _key(0x01, 33);
        bytes memory k2 = _key(0x02, 33);
        (address acct,) = _newAccountWithKey(k1);

        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _updatePayload(_singleKey(k2), _singleKey(k2));

        vm.expectRevert(abi.encodeWithSelector(IWorldIDAccountManager.OverlappingUpdateKey.selector, keccak256(k2)));
        manager.update(acct, 1, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), u);
    }

    function test_update_remove_revertIf_unknownKey() public {
        (address acct,) = _newAccountWithKey(_key(0x01, 33));
        bytes memory k2 = _key(0x02, 33);
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _updatePayload(_emptyKeys(), _singleKey(k2));
        vm.expectRevert(abi.encodeWithSelector(IWorldIDAccountManager.UnknownSessionKey.selector, keccak256(k2)));
        manager.update(acct, 1, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), u);
    }

    function test_update_remove_canDrainSetAndReadd() public {
        bytes memory k1 = _key(0x01, 33);
        (address acct,) = _newAccountWithKey(k1);

        IWorldIDAccountManager.WorldIDAccountUpdate memory rm = _updatePayload(_emptyKeys(), _singleKey(k1));
        manager.update(acct, 1, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), rm);
        assertEq(manager.getSessionKeyHashes(acct).length, 0);
        assertFalse(manager.isAuthorized(acct, k1));

        // Account still exists.
        assertEq(manager.worldIDAccountNullifier(acct), 0x1234567890ABCDEF);

        IWorldIDAccountManager.WorldIDAccountUpdate memory addBack = _updatePayload(_singleKey(k1), _emptyKeys());
        manager.update(acct, 2, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), addBack);
        assertTrue(manager.isAuthorized(acct, k1));
    }

    function test_update_signalHashChangesWithGeneration() public {
        // Identical payloads at generation 0 and generation 2 must produce different signal hashes.
        bytes memory k1 = _key(0x01, 33);
        bytes memory k2 = _key(0x02, 33);
        (address acct,) = _newAccountWithKey(k1);

        IWorldIDAccountManager.WorldIDAccountUpdate memory addK2 = _updatePayload(_singleKey(k2), _emptyKeys());
        IWorldIDAccountManager.WorldIDAccountUpdate memory removeK2 = _updatePayload(_emptyKeys(), _singleKey(k2));

        uint256 firstHash = _signalHashUpdate(addK2, 0);
        uint256 secondHash = _signalHashUpdate(addK2, 2);
        assertTrue(firstHash != secondHash, "signal hash must change with generation");

        manager.update(acct, 1, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), addK2);
        manager.update(acct, 2, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), removeK2);
        assertEq(manager.generationOf(acct), 2);

        // Now the addK2 payload would bind to generation = 2, and the verifier should see secondHash.
        vm.expectCall(
            address(verifier),
            abi.encodeCall(
                IWorldIDVerifier.verifySession,
                (
                    WORLD_CHAIN_RP_ID,
                    3,
                    secondHash,
                    uint64(block.timestamp + 1 days),
                    uint64(1),
                    0,
                    0xDEADBEEF,
                    _sessionNullifier(),
                    _proof()
                )
            )
        );
        manager.update(acct, 3, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), addK2);
    }

    function test_update_revertIf_emptyDelta() public {
        (address acct,) = _newAccountWithKey(_key(0x01, 33));
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _updatePayload(_emptyKeys(), _emptyKeys());
        vm.expectRevert(IWorldIDAccountManager.EmptyKeySet.selector);
        manager.update(acct, 1, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), u);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              isAuthorized                               ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_isAuthorized_falseForBadLengths_andUnknownAccount() public {
        bytes memory k = _key(0xAA, 33);
        (address acct,) = _newAccountWithKey(k);

        bytes memory zero = new bytes(0);
        bytes memory tooLong = _key(0x01, 129);
        assertFalse(manager.isAuthorized(acct, zero));
        assertFalse(manager.isAuthorized(acct, tooLong));
        assertFalse(manager.isAuthorized(address(0xDEAD), k), "no entries for nonexistent acct");
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                 ADMIN                                   ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_setWorldIDVerifier_onlyOwner() public {
        MockWorldIDVerifier newVerifier = new MockWorldIDVerifier(true);

        vm.prank(ATTACKER);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, ATTACKER));
        manager.setWorldIDVerifier(newVerifier);

        vm.prank(OWNER);
        vm.expectEmit(true, true, true, true, address(manager));
        emit WorldIDVerifierSet(address(newVerifier));
        manager.setWorldIDVerifier(newVerifier);
        assertEq(address(manager.worldIDVerifier()), address(newVerifier));
    }

    function test_setWorldIDVerifier_revertIf_zeroAddress() public {
        vm.prank(OWNER);
        vm.expectRevert(IWorldIDAccountManager.AddressZero.selector);
        manager.setWorldIDVerifier(IWorldIDVerifier(address(0)));
    }

    function test_initialize_revertIf_zeroVerifier() public {
        WorldIDAccountManagerImplV1 freshImpl = new WorldIDAccountManagerImplV1();
        bytes memory initCall =
            abi.encodeCall(WorldIDAccountManagerImplV1.initialize, (IWorldIDVerifier(address(0)), OWNER));
        vm.expectRevert(IWorldIDAccountManager.AddressZero.selector);
        new WorldIDAccountManager(address(freshImpl), initCall);
    }

    function test_initialize_revertIf_zeroOwner() public {
        WorldIDAccountManagerImplV1 freshImpl = new WorldIDAccountManagerImplV1();
        bytes memory initCall =
            abi.encodeCall(WorldIDAccountManagerImplV1.initialize, (IWorldIDVerifier(address(verifier)), address(0)));
        vm.expectRevert(IWorldIDAccountManager.AddressZero.selector);
        new WorldIDAccountManager(address(freshImpl), initCall);
    }

    function test_implementation_cannotBeInitializedDirectly() public {
        WorldIDAccountManagerImplV1 freshImpl = new WorldIDAccountManagerImplV1();
        // Initializable.InvalidInitialization() selector
        vm.expectRevert(bytes4(0xf92ee8a9));
        freshImpl.initialize(verifier, OWNER);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              UUPS UPGRADE                               ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_uupsUpgrade_preservesState() public {
        bytes memory k = _key(0xAA, 33);
        (address acct,) = _newAccountWithKey(k);
        uint256 sid = manager.sessionIdOf(acct);

        WorldIDAccountManagerImplV1 newImpl = new WorldIDAccountManagerImplV1();
        vm.prank(OWNER);
        manager.upgradeToAndCall(address(newImpl), bytes(""));

        assertEq(manager.sessionIdOf(acct), sid, "sessionId preserved");
        assertTrue(manager.isAuthorized(acct, k), "key set preserved");
    }

    function test_uupsUpgrade_onlyOwner() public {
        WorldIDAccountManagerImplV1 newImpl = new WorldIDAccountManagerImplV1();
        vm.prank(ATTACKER);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, ATTACKER));
        manager.upgradeToAndCall(address(newImpl), bytes(""));
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  FUZZ                                   ///
    ///////////////////////////////////////////////////////////////////////////////

    function testFuzz_create_thenAuthorize(uint256 nullifier_, uint256 nonce_, bytes calldata rawKey_) public {
        vm.assume(nullifier_ != 0);
        vm.assume(rawKey_.length > 0 && rawKey_.length <= 128);

        CreateArgs memory a = _defaultCreateArgs();
        a.nullifier = nullifier_;
        a.nonce = nonce_;
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _createUpdate(_singleKey(rawKey_));

        address expected = _accountAddress(nullifier_);
        vm.assume(manager.worldIDAccountNullifier(expected) == 0); // collision guard for fuzz

        address acct = _doCreate(a, u);
        assertEq(acct, expected);
        assertTrue(manager.isAuthorized(acct, rawKey_));
        assertEq(_action(nonce_) >> 248, 0);
    }

    function testFuzz_action_alwaysFitsBelowTopByte(uint256 nonce_) public pure {
        assertEq(_action(nonce_) >> 248, 0);
    }
}
