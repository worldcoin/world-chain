// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";

import {WorldIDAccountManager} from "../src/WorldIDAccountManager.sol";
import {WorldIDAccountManagerImplV1} from "../src/WorldIDAccountManagerImplV1.sol";
import {IWorldIDAccountManager} from "../src/interfaces/IWorldIDAccountManager.sol";
import {IWorldIDVerifier} from "../src/interfaces/IWorldIDVerifier.sol";
import {MockWorldIDVerifier} from "./mocks/MockWorldIDVerifier.sol";

/// @title WorldIDAccountManagerImplV1 tests
/// @notice Exercises the delayed-activation key-update flow, including the effective/stored split.
contract WorldIDAccountManagerImplV1Test is Test {
    WorldIDAccountManagerImplV1 internal manager;
    WorldIDAccountManagerImplV1 internal impl;
    MockWorldIDVerifier internal verifier;

    address internal constant OWNER = address(0xC0FFEE);
    address internal constant ATTACKER = address(0xBAD);
    uint256 internal constant COOLDOWN = 3 days;

    bytes16 internal constant WORLD_ID_ACCOUNT_TAG = "WORLD_ID_ACCOUNT";
    uint64 internal constant WORLD_CHAIN_RP_ID = 480;

    event WorldIDAccountCreated(
        address indexed worldIDAccount, uint256 indexed worldIDAccountNullifier, uint256 indexed sessionId
    );
    event SessionKeysAdded(address indexed worldIDAccount, bytes[] keys);
    event SessionKeyUpdateScheduled(
        address indexed worldIDAccount, bytes[] addKeys, bytes[] removeKeys, uint256 validAfter
    );
    event SessionKeyUpdateReverted(address indexed worldIDAccount);
    event WorldIDVerifierSet(address indexed worldIDVerifier);
    event SessionKeyUpdateCooldownSet(uint256 cooldown);

    function setUp() public {
        verifier = new MockWorldIDVerifier(true);
        impl = new WorldIDAccountManagerImplV1();
        bytes memory initCall = abi.encodeCall(WorldIDAccountManagerImplV1.initialize, (verifier, OWNER, COOLDOWN));
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

    function _signalHashRevert(uint256 validAfter_, uint256 generation_) internal pure returns (uint256) {
        return uint256(keccak256(abi.encode(IWorldIDAccountManager.Operation.Revert, validAfter_, generation_))) >> 8;
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

    function _createUpdate(bytes[] memory addKeys_)
        internal
        pure
        returns (IWorldIDAccountManager.WorldIDAccountUpdate memory u)
    {
        u = IWorldIDAccountManager.WorldIDAccountUpdate({
            operation: IWorldIDAccountManager.Operation.Create, addKeys: addKeys_, removeKeys: _emptyKeys()
        });
    }

    function _updatePayload(bytes[] memory addKeys_, bytes[] memory removeKeys_)
        internal
        pure
        returns (IWorldIDAccountManager.WorldIDAccountUpdate memory u)
    {
        u = IWorldIDAccountManager.WorldIDAccountUpdate({
            operation: IWorldIDAccountManager.Operation.Update, addKeys: addKeys_, removeKeys: removeKeys_
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
        acct = _doCreate(a, _createUpdate(_singleKey(firstKey_)));
    }

    function _assertHashesEq(bytes32[] memory actual_, bytes32[] memory expected_) internal pure {
        assertEq(actual_.length, expected_.length, "hash array length mismatch");
        for (uint256 i = 0; i < actual_.length; ++i) {
            assertEq(actual_[i], expected_[i], "hash mismatch");
        }
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

        assertEq(acct, expectedAcct);
        assertEq(manager.worldIDAccountNullifier(acct), a.nullifier);
        assertEq(manager.sessionIdOf(acct), a.sessionId);
        assertEq(manager.generationOf(acct), 0);
        assertEq(manager.sessionKeyUpdateCooldown(), COOLDOWN);

        bytes32[] memory expected = new bytes32[](1);
        expected[0] = keccak256(k);
        _assertHashesEq(manager.getSessionKeyHashes(acct), expected);
        _assertHashesEq(manager.getStoredSessionKeyHashes(acct), expected);
        assertEq(manager.getPreviousSessionKeyHashes(acct).length, 0);
        assertEq(manager.getPendingValidAfter(acct), 0);
        assertTrue(manager.isAuthorized(acct, k));
    }

    function test_create_revertIf_wrongOperation() public {
        CreateArgs memory a = _defaultCreateArgs();
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _updatePayload(_singleKey(_key(0x01, 33)), _emptyKeys());
        vm.expectRevert(IWorldIDAccountManager.InvalidOperation.selector);
        _doCreate(a, u);
    }

    function test_create_revertIf_verifierRejects() public {
        verifier.setShouldAccept(false);
        CreateArgs memory a = _defaultCreateArgs();
        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _createUpdate(_singleKey(_key(0x01, 33)));
        vm.expectRevert(MockWorldIDVerifier.MockVerifierRejected.selector);
        _doCreate(a, u);
        assertEq(manager.worldIDAccountNullifier(_accountAddress(a.nullifier)), 0);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                 UPDATE                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_update_schedulesStoredKeysButEffectiveRemainsOld() public {
        bytes memory k1 = _key(0x01, 33);
        bytes memory k2 = _key(0x02, 33);
        (address acct,) = _newAccountWithKey(k1);

        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _updatePayload(_singleKey(k2), _emptyKeys());
        uint256 validAfter = block.timestamp + COOLDOWN;

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
        emit SessionKeyUpdateScheduled(acct, u.addKeys, u.removeKeys, validAfter);

        manager.update(acct, 777, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), u);

        bytes32[] memory effective = new bytes32[](1);
        effective[0] = keccak256(k1);
        bytes32[] memory stored = new bytes32[](2);
        stored[0] = keccak256(k1);
        stored[1] = keccak256(k2);

        assertEq(manager.generationOf(acct), 1);
        _assertHashesEq(manager.getSessionKeyHashes(acct), effective);
        _assertHashesEq(manager.getStoredSessionKeyHashes(acct), stored);
        _assertHashesEq(manager.getPreviousSessionKeyHashes(acct), effective);
        assertEq(manager.getPendingValidAfter(acct), validAfter);
        assertTrue(manager.isAuthorized(acct, k1));
        assertFalse(manager.isAuthorized(acct, k2));
    }

    function test_update_afterCooldown_newKeysBecomeEffective() public {
        bytes memory k1 = _key(0x01, 33);
        bytes memory k2 = _key(0x02, 33);
        bytes memory k3 = _key(0x03, 33);

        CreateArgs memory a = _defaultCreateArgs();
        bytes[] memory initial = new bytes[](2);
        initial[0] = k1;
        initial[1] = k2;
        address acct = _doCreate(a, _createUpdate(initial));

        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _updatePayload(_singleKey(k3), _singleKey(k1));
        manager.update(acct, 1, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), u);

        assertTrue(manager.isAuthorized(acct, k1));
        assertTrue(manager.isAuthorized(acct, k2));
        assertFalse(manager.isAuthorized(acct, k3));

        vm.warp(block.timestamp + COOLDOWN);

        bytes32[] memory expected = new bytes32[](2);
        expected[0] = keccak256(k2);
        expected[1] = keccak256(k3);

        _assertHashesEq(manager.getSessionKeyHashes(acct), expected);
        _assertHashesEq(manager.getStoredSessionKeyHashes(acct), expected);
        assertEq(manager.getPreviousSessionKeyHashes(acct).length, 0);
        assertEq(manager.getPendingValidAfter(acct), 0);
        assertFalse(manager.isAuthorized(acct, k1));
        assertTrue(manager.isAuthorized(acct, k2));
        assertTrue(manager.isAuthorized(acct, k3));
    }

    function test_update_revertIf_activePendingExists() public {
        bytes memory k1 = _key(0x01, 33);
        bytes memory k2 = _key(0x02, 33);
        bytes memory k3 = _key(0x03, 33);
        (address acct,) = _newAccountWithKey(k1);

        manager.update(
            acct,
            1,
            1,
            uint64(block.timestamp + 1 days),
            0,
            _sessionNullifier(),
            _proof(),
            _updatePayload(_singleKey(k2), _emptyKeys())
        );

        vm.expectRevert(
            abi.encodeWithSelector(IWorldIDAccountManager.PendingSessionKeyUpdate.selector, block.timestamp + COOLDOWN)
        );
        manager.update(
            acct,
            2,
            1,
            uint64(block.timestamp + 1 days),
            0,
            _sessionNullifier(),
            _proof(),
            _updatePayload(_singleKey(k3), _emptyKeys())
        );
    }

    function test_update_removeAllKeys_oldKeyRemainsEffectiveUntilCooldown() public {
        bytes memory k1 = _key(0x01, 33);
        (address acct,) = _newAccountWithKey(k1);

        manager.update(
            acct,
            1,
            1,
            uint64(block.timestamp + 1 days),
            0,
            _sessionNullifier(),
            _proof(),
            _updatePayload(_emptyKeys(), _singleKey(k1))
        );

        assertTrue(manager.isAuthorized(acct, k1));
        assertEq(manager.getSessionKeyHashes(acct).length, 1);
        assertEq(manager.getStoredSessionKeyHashes(acct).length, 0);

        vm.warp(block.timestamp + COOLDOWN);

        assertFalse(manager.isAuthorized(acct, k1));
        assertEq(manager.getSessionKeyHashes(acct).length, 0);
    }

    function test_update_signalHashUsesGenerationAndExpiredWindowCanBeRescheduled() public {
        bytes memory k1 = _key(0x01, 33);
        bytes memory k2 = _key(0x02, 33);
        bytes memory k3 = _key(0x03, 33);
        (address acct,) = _newAccountWithKey(k1);

        IWorldIDAccountManager.WorldIDAccountUpdate memory first = _updatePayload(_singleKey(k2), _emptyKeys());
        manager.update(acct, 1, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), first);
        assertEq(manager.generationOf(acct), 1);

        vm.warp(block.timestamp + COOLDOWN + 1);

        IWorldIDAccountManager.WorldIDAccountUpdate memory second = _updatePayload(_singleKey(k3), _singleKey(k1));
        vm.expectCall(
            address(verifier),
            abi.encodeCall(
                IWorldIDVerifier.verifySession,
                (
                    WORLD_CHAIN_RP_ID,
                    2,
                    _signalHashUpdate(second, 1),
                    uint64(block.timestamp + 1 days),
                    uint64(1),
                    0,
                    0xDEADBEEF,
                    _sessionNullifier(),
                    _proof()
                )
            )
        );
        manager.update(acct, 2, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), second);
        assertEq(manager.generationOf(acct), 2);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                 REVERT                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_revert_restoresPreviousStoredAndEffectiveKeys() public {
        bytes memory k1 = _key(0x01, 33);
        bytes memory k2 = _key(0x02, 33);
        bytes memory k3 = _key(0x03, 33);

        CreateArgs memory a = _defaultCreateArgs();
        bytes[] memory initial = new bytes[](2);
        initial[0] = k1;
        initial[1] = k2;
        address acct = _doCreate(a, _createUpdate(initial));

        IWorldIDAccountManager.WorldIDAccountUpdate memory u = _updatePayload(_singleKey(k3), _singleKey(k1));
        manager.update(acct, 1, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof(), u);
        uint256 validAfter = block.timestamp + COOLDOWN;

        vm.expectCall(
            address(verifier),
            abi.encodeCall(
                IWorldIDVerifier.verifySession,
                (
                    WORLD_CHAIN_RP_ID,
                    2,
                    _signalHashRevert(validAfter, 1),
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
        emit SessionKeyUpdateReverted(acct);

        manager.revert(acct, 2, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof());

        bytes32[] memory expected = new bytes32[](2);
        expected[0] = keccak256(k1);
        expected[1] = keccak256(k2);

        assertEq(manager.generationOf(acct), 2);
        _assertHashesEq(manager.getSessionKeyHashes(acct), expected);
        _assertHashesEq(manager.getStoredSessionKeyHashes(acct), expected);
        assertEq(manager.getPreviousSessionKeyHashes(acct).length, 0);
        assertEq(manager.getPendingValidAfter(acct), 0);
        assertTrue(manager.isAuthorized(acct, k1));
        assertTrue(manager.isAuthorized(acct, k2));
        assertFalse(manager.isAuthorized(acct, k3));
    }

    function test_revert_revertIf_noPending() public {
        (address acct,) = _newAccountWithKey(_key(0x01, 33));
        vm.expectRevert(IWorldIDAccountManager.NoPendingSessionKeyUpdate.selector);
        manager.revert(acct, 1, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof());
    }

    function test_revert_revertIf_windowExpired() public {
        bytes memory k1 = _key(0x01, 33);
        bytes memory k2 = _key(0x02, 33);
        (address acct,) = _newAccountWithKey(k1);

        manager.update(
            acct,
            1,
            1,
            uint64(block.timestamp + 1 days),
            0,
            _sessionNullifier(),
            _proof(),
            _updatePayload(_singleKey(k2), _emptyKeys())
        );
        uint256 validAfter = block.timestamp + COOLDOWN;

        vm.warp(validAfter);

        vm.expectRevert(
            abi.encodeWithSelector(IWorldIDAccountManager.PendingSessionKeyUpdateExpired.selector, validAfter)
        );
        manager.revert(acct, 2, 1, uint64(block.timestamp + 1 days), 0, _sessionNullifier(), _proof());
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                 VIEWS                                   ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_isAuthorized_falseForBadLengths_andUnknownAccount() public {
        bytes memory k = _key(0xAA, 33);
        (address acct,) = _newAccountWithKey(k);

        bytes memory zero = new bytes(0);
        bytes memory tooLong = _key(0x01, 129);
        assertFalse(manager.isAuthorized(acct, zero));
        assertFalse(manager.isAuthorized(acct, tooLong));
        assertFalse(manager.isAuthorized(address(0xDEAD), k));
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

    function test_setSessionKeyUpdateCooldown_onlyOwner_andNonZero() public {
        vm.prank(ATTACKER);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, ATTACKER));
        manager.setSessionKeyUpdateCooldown(7 days);

        vm.prank(OWNER);
        vm.expectRevert(IWorldIDAccountManager.ZeroCooldown.selector);
        manager.setSessionKeyUpdateCooldown(0);

        vm.prank(OWNER);
        vm.expectEmit(false, false, false, true, address(manager));
        emit SessionKeyUpdateCooldownSet(7 days);
        manager.setSessionKeyUpdateCooldown(7 days);

        assertEq(manager.sessionKeyUpdateCooldown(), 7 days);
    }

    function test_initialize_revertIf_zeroVerifier_zeroOwner_orZeroCooldown() public {
        WorldIDAccountManagerImplV1 freshImpl = new WorldIDAccountManagerImplV1();

        bytes memory zeroVerifier =
            abi.encodeCall(WorldIDAccountManagerImplV1.initialize, (IWorldIDVerifier(address(0)), OWNER, COOLDOWN));
        vm.expectRevert(IWorldIDAccountManager.AddressZero.selector);
        new WorldIDAccountManager(address(freshImpl), zeroVerifier);

        freshImpl = new WorldIDAccountManagerImplV1();
        bytes memory zeroOwner = abi.encodeCall(
            WorldIDAccountManagerImplV1.initialize, (IWorldIDVerifier(address(verifier)), address(0), COOLDOWN)
        );
        vm.expectRevert(IWorldIDAccountManager.AddressZero.selector);
        new WorldIDAccountManager(address(freshImpl), zeroOwner);

        freshImpl = new WorldIDAccountManagerImplV1();
        bytes memory zeroCooldown =
            abi.encodeCall(WorldIDAccountManagerImplV1.initialize, (IWorldIDVerifier(address(verifier)), OWNER, 0));
        vm.expectRevert(IWorldIDAccountManager.ZeroCooldown.selector);
        new WorldIDAccountManager(address(freshImpl), zeroCooldown);
    }

    function test_implementation_cannotBeInitializedDirectly() public {
        WorldIDAccountManagerImplV1 freshImpl = new WorldIDAccountManagerImplV1();
        vm.expectRevert(bytes4(0xf92ee8a9));
        freshImpl.initialize(verifier, OWNER, COOLDOWN);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              UUPS UPGRADE                               ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_uupsUpgrade_preservesPendingState() public {
        bytes memory k1 = _key(0x01, 33);
        bytes memory k2 = _key(0x02, 33);
        (address acct,) = _newAccountWithKey(k1);

        manager.update(
            acct,
            1,
            1,
            uint64(block.timestamp + 1 days),
            0,
            _sessionNullifier(),
            _proof(),
            _updatePayload(_singleKey(k2), _emptyKeys())
        );
        uint256 validAfter = manager.getPendingValidAfter(acct);

        WorldIDAccountManagerImplV1 newImpl = new WorldIDAccountManagerImplV1();
        vm.prank(OWNER);
        manager.upgradeToAndCall(address(newImpl), bytes(""));

        assertEq(manager.getPendingValidAfter(acct), validAfter);
        assertTrue(manager.isAuthorized(acct, k1));
        assertFalse(manager.isAuthorized(acct, k2));
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

        address expected = _accountAddress(nullifier_);
        vm.assume(manager.worldIDAccountNullifier(expected) == 0);

        address acct = _doCreate(a, _createUpdate(_singleKey(rawKey_)));
        assertEq(acct, expected);
        assertTrue(manager.isAuthorized(acct, rawKey_));
        assertEq(_action(nonce_) >> 248, 0);
    }
}
