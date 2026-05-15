// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test, Vm} from "forge-std/Test.sol";

import {WorldChainAccountManager} from "../src/WorldChainAccountManager.sol";
import {WorldChainAccountManagerImplV1} from "../src/WorldChainAccountManagerImplV1.sol";
import {WorldChainAccountRouter} from "../src/WorldChainAccountRouter.sol";
import {IWorldChainAccountManager} from "../src/interfaces/IWorldChainAccountManager.sol";
import {IWorldChainAccountRouter} from "../src/interfaces/IWorldChainAccountRouter.sol";
import {WorldChainAccountConstants} from "../src/libraries/WorldChainAccountConstants.sol";
import {WorldChainAccountHashes} from "../src/libraries/WorldChainAccountHashes.sol";
import {
    AccountAlreadyExists,
    AccountDoesNotExist,
    ActivationParametersAlreadyFrozen,
    AddressZero,
    AdminAuthorizationFailed,
    AdminAuthorizationTooLarge,
    AdminVerifierZero,
    DuplicateSessionVerifier,
    EmptySessionVerifierSet,
    InstallationTooLarge,
    KeyRingUnchanged,
    ManagerOnly,
    SessionVerifierZero,
    StaleKeyRingHash,
    TooManySessionVerifiers,
    UnauthorizedSessionVerifier,
    VerifierUndeployed
} from "../src/libraries/WorldChainAccountErrors.sol";
import {MockAdminVerifier} from "./mocks/MockAdminVerifier.sol";
import {MockSessionVerifier} from "./mocks/MockSessionVerifier.sol";

/// @title WorldChainAccountManagerImplV1 — full test suite
contract WorldChainAccountManagerImplV1Test is Test {
    address internal constant OWNER = address(uint160(uint256(keccak256("owner"))));

    WorldChainAccountManagerImplV1 internal manager;
    MockAdminVerifier internal adminVerifier;
    MockSessionVerifier internal sessionVerifierA;
    MockSessionVerifier internal sessionVerifierB;

    bytes internal constant ACCEPT_SIG = hex"01";
    bytes internal constant REJECT_SIG = hex"00";

    function setUp() public {
        WorldChainAccountManagerImplV1 impl = new WorldChainAccountManagerImplV1();
        bytes memory initCall = abi.encodeCall(WorldChainAccountManagerImplV1.initialize, (OWNER));
        manager = WorldChainAccountManagerImplV1(address(new WorldChainAccountManager(address(impl), initCall)));

        adminVerifier = new MockAdminVerifier();
        sessionVerifierA = new MockSessionVerifier();
        sessionVerifierB = new MockSessionVerifier();
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              HELPERS                                    ///
    ///////////////////////////////////////////////////////////////////////////////

    function _admin(bytes memory installation)
        internal
        view
        returns (IWorldChainAccountManager.WorldChainAccountVerifier memory)
    {
        return IWorldChainAccountManager.WorldChainAccountVerifier({
            verifier: address(adminVerifier), installation: installation
        });
    }

    function _session(address verifier, bytes memory installation)
        internal
        pure
        returns (IWorldChainAccountManager.WorldChainAccountVerifier memory)
    {
        return IWorldChainAccountManager.WorldChainAccountVerifier({verifier: verifier, installation: installation});
    }

    function _sessions1() internal view returns (IWorldChainAccountManager.WorldChainAccountVerifier[] memory s) {
        s = new IWorldChainAccountManager.WorldChainAccountVerifier[](1);
        s[0] = _session(address(sessionVerifierA), hex"");
    }

    function _sessions2() internal view returns (IWorldChainAccountManager.WorldChainAccountVerifier[] memory s) {
        s = new IWorldChainAccountManager.WorldChainAccountVerifier[](2);
        s[0] = _session(address(sessionVerifierA), hex"");
        s[1] = _session(address(sessionVerifierB), hex"");
    }

    function _expectedAccount(bytes32 accountSalt) internal view returns (address) {
        IWorldChainAccountManager.WorldChainAccountVerifier memory admin = _admin(hex"");
        return WorldChainAccountHashes.deriveAccount(
            block.chainid,
            address(manager),
            manager.getAccountRouterCodeHash(),
            WorldChainAccountHashes.adminHash(admin),
            accountSalt
        );
    }

    function _createDefault(bytes32 accountSalt) internal returns (address account) {
        account = manager.create(_admin(hex""), accountSalt, _sessions1(), ACCEPT_SIG);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              CREATE — HAPPY                             ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_create_singleSessionHappyPath() public {
        bytes32 salt = bytes32(uint256(1));
        address expected = _expectedAccount(salt);

        vm.recordLogs();
        address account = manager.create(_admin(hex""), salt, _sessions1(), ACCEPT_SIG);

        assertEq(account, expected, "account address must match spec-derived value");
        assertEq(manager.getAdminNonce(account), 0);
        assertEq(manager.getTransactionNonce(account), 0);
        assertEq(manager.getKeyRingHash(account), WorldChainAccountHashes.keyRingHash(_sessions1()));
        assertTrue(manager.isAuthorizedSessionVerifier(account, address(sessionVerifierA)));

        IWorldChainAccountManager.WorldChainAccountVerifier memory admin = manager.getAdmin(account);
        assertEq(admin.verifier, address(adminVerifier));
        assertEq(admin.installation.length, 0);

        Vm.Log[] memory logs = vm.getRecordedLogs();
        bool sawAccountCreated;
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].topics[0] == keccak256("AccountCreated(address,address,bytes32,bytes32,bytes32)")) {
                sawAccountCreated = true;
                assertEq(address(uint160(uint256(logs[i].topics[1]))), account, "indexed account mismatch");
                assertEq(address(uint160(uint256(logs[i].topics[2]))), address(adminVerifier));
            }
        }
        assertTrue(sawAccountCreated, "AccountCreated must be emitted");
    }

    function test_create_multiSessionAndOrderingPreserved() public {
        IWorldChainAccountManager.WorldChainAccountVerifier[] memory s = _sessions2();
        address account = manager.create(_admin(hex""), bytes32(uint256(2)), s, ACCEPT_SIG);

        IWorldChainAccountManager.WorldChainAccountVerifier[] memory stored = manager.getSessionVerifiers(account);
        assertEq(stored.length, 2);
        assertEq(stored[0].verifier, address(sessionVerifierA));
        assertEq(stored[1].verifier, address(sessionVerifierB));
        assertEq(manager.getKeyRingHash(account), WorldChainAccountHashes.keyRingHash(s));
    }

    function test_create_deploysRouterAtSaltedAddress() public {
        address account = _createDefault(bytes32(uint256(3)));
        address router = manager.getAccountRouter(account);
        assertGt(router.code.length, 0, "router must be deployed");
        assertEq(IWorldChainAccountRouter(router).manager(), address(manager));
        assertEq(IWorldChainAccountRouter(router).adminVerifier(), address(adminVerifier));
        assertEq(keccak256(router.code), manager.getAccountRouterCodeHash(), "router runtime hash must be canonical");
    }

    function test_create_callsInstallExactlyOnceOnAdminAndSession() public {
        address account = _createDefault(bytes32(uint256(4)));
        address router = manager.getAccountRouter(account);

        // The router's storage now hosts the mocks' install counters under their namespaced
        // slots. Read them by computing the same slot the mocks use.
        bytes32 adminSlot = keccak256(abi.encode("MockAdminVerifier.installs", address(adminVerifier)));
        bytes32 sessionSlot = keccak256(abi.encode("MockSessionVerifier.installs", address(sessionVerifierA)));
        bytes32 adminVal = vm.load(router, adminSlot);
        bytes32 sessionVal = vm.load(router, sessionSlot);
        assertEq(uint256(adminVal), 1, "admin install counter == 1");
        assertEq(uint256(sessionVal), 1, "session install counter == 1");
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                            CREATE — REJECTIONS                          ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_create_revertsOnZeroAdminVerifier() public {
        IWorldChainAccountManager.WorldChainAccountVerifier memory admin =
            IWorldChainAccountManager.WorldChainAccountVerifier({verifier: address(0), installation: hex""});
        vm.expectRevert(AdminVerifierZero.selector);
        manager.create(admin, bytes32(uint256(1)), _sessions1(), ACCEPT_SIG);
    }

    function test_create_revertsOnUndeployedAdminVerifier() public {
        IWorldChainAccountManager.WorldChainAccountVerifier memory admin =
            IWorldChainAccountManager.WorldChainAccountVerifier({verifier: address(0xdead), installation: hex""});
        vm.expectRevert(abi.encodeWithSelector(VerifierUndeployed.selector, address(0xdead)));
        manager.create(admin, bytes32(uint256(1)), _sessions1(), ACCEPT_SIG);
    }

    function test_create_revertsOnZeroSessionVerifier() public {
        IWorldChainAccountManager.WorldChainAccountVerifier[] memory s =
            new IWorldChainAccountManager.WorldChainAccountVerifier[](1);
        s[0] = _session(address(0), hex"");
        vm.expectRevert(SessionVerifierZero.selector);
        manager.create(_admin(hex""), bytes32(uint256(1)), s, ACCEPT_SIG);
    }

    function test_create_revertsOnUndeployedSessionVerifier() public {
        IWorldChainAccountManager.WorldChainAccountVerifier[] memory s =
            new IWorldChainAccountManager.WorldChainAccountVerifier[](1);
        s[0] = _session(address(0xbeef), hex"");
        vm.expectRevert(abi.encodeWithSelector(VerifierUndeployed.selector, address(0xbeef)));
        manager.create(_admin(hex""), bytes32(uint256(1)), s, ACCEPT_SIG);
    }

    function test_create_revertsOnDuplicateSessionVerifier() public {
        IWorldChainAccountManager.WorldChainAccountVerifier[] memory s =
            new IWorldChainAccountManager.WorldChainAccountVerifier[](2);
        s[0] = _session(address(sessionVerifierA), hex"");
        s[1] = _session(address(sessionVerifierA), hex"");
        vm.expectRevert(abi.encodeWithSelector(DuplicateSessionVerifier.selector, address(sessionVerifierA)));
        manager.create(_admin(hex""), bytes32(uint256(1)), s, ACCEPT_SIG);
    }

    function test_create_revertsOnEmptySessionSet() public {
        IWorldChainAccountManager.WorldChainAccountVerifier[] memory s =
            new IWorldChainAccountManager.WorldChainAccountVerifier[](0);
        vm.expectRevert(EmptySessionVerifierSet.selector);
        manager.create(_admin(hex""), bytes32(uint256(1)), s, ACCEPT_SIG);
    }

    function test_create_revertsOnTooManySessions() public {
        uint256 max = WorldChainAccountConstants.MAX_SESSION_VERIFIERS;
        IWorldChainAccountManager.WorldChainAccountVerifier[] memory s =
            new IWorldChainAccountManager.WorldChainAccountVerifier[](max + 1);
        for (uint256 i; i < s.length; ++i) {
            s[i] = _session(address(new MockSessionVerifier()), hex"");
        }
        vm.expectRevert(abi.encodeWithSelector(TooManySessionVerifiers.selector, max + 1, max));
        manager.create(_admin(hex""), bytes32(uint256(1)), s, ACCEPT_SIG);
    }

    function test_create_revertsOnOversizedInstallation() public {
        uint32 cap = manager.maxVerifierInstallDataBytes();
        bytes memory oversized = new bytes(uint256(cap) + 1);
        vm.expectRevert(abi.encodeWithSelector(InstallationTooLarge.selector, oversized.length, cap));
        manager.create(_admin(oversized), bytes32(uint256(1)), _sessions1(), ACCEPT_SIG);
    }

    function test_create_revertsOnOversizedAdminAuthorization() public {
        uint32 cap = manager.maxAdminAuthorizationBytes();
        bytes memory oversized = new bytes(uint256(cap) + 1);
        vm.expectRevert(abi.encodeWithSelector(AdminAuthorizationTooLarge.selector, oversized.length, cap));
        manager.create(_admin(hex""), bytes32(uint256(1)), _sessions1(), oversized);
    }

    function test_create_revertsOnFailedAdminAuthorization() public {
        vm.expectRevert(AdminAuthorizationFailed.selector);
        manager.create(_admin(hex""), bytes32(uint256(1)), _sessions1(), REJECT_SIG);
    }

    function test_create_revertsOnDuplicateAccount() public {
        bytes32 salt = bytes32(uint256(7));
        address account = _createDefault(salt);
        vm.expectRevert(abi.encodeWithSelector(AccountAlreadyExists.selector, account));
        _createDefault(salt);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                          SET KEY RING — HAPPY                           ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_setKeyRing_replacesSetAndIncrementsNonce() public {
        address account = _createDefault(bytes32(uint256(11)));
        bytes32 currentHash = manager.getKeyRingHash(account);

        manager.setKeyRing(account, currentHash, _sessions2(), ACCEPT_SIG);

        assertEq(manager.getAdminNonce(account), 1);
        assertEq(manager.getKeyRingHash(account), WorldChainAccountHashes.keyRingHash(_sessions2()));
        IWorldChainAccountManager.WorldChainAccountVerifier[] memory s = manager.getSessionVerifiers(account);
        assertEq(s.length, 2);
        assertTrue(manager.isAuthorizedSessionVerifier(account, address(sessionVerifierA)));
        assertTrue(manager.isAuthorizedSessionVerifier(account, address(sessionVerifierB)));
    }

    function test_setKeyRing_clearsRemovedVerifierFromAuthorization() public {
        address account = manager.create(_admin(hex""), bytes32(uint256(12)), _sessions2(), ACCEPT_SIG);
        bytes32 currentHash = manager.getKeyRingHash(account);

        // Replace with only A (drop B).
        manager.setKeyRing(account, currentHash, _sessions1(), ACCEPT_SIG);

        assertTrue(manager.isAuthorizedSessionVerifier(account, address(sessionVerifierA)));
        assertFalse(manager.isAuthorizedSessionVerifier(account, address(sessionVerifierB)));
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                       SET KEY RING — REJECTIONS                         ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_setKeyRing_revertsWhenAccountMissing() public {
        vm.expectRevert(abi.encodeWithSelector(AccountDoesNotExist.selector, address(0xdead)));
        manager.setKeyRing(address(0xdead), bytes32(0), _sessions2(), ACCEPT_SIG);
    }

    function test_setKeyRing_revertsOnStaleHash() public {
        address account = _createDefault(bytes32(uint256(13)));
        bytes32 wrong = keccak256("stale");
        vm.expectRevert(abi.encodeWithSelector(StaleKeyRingHash.selector, wrong, manager.getKeyRingHash(account)));
        manager.setKeyRing(account, wrong, _sessions2(), ACCEPT_SIG);
    }

    function test_setKeyRing_revertsOnUnchangedSet() public {
        address account = _createDefault(bytes32(uint256(14)));
        bytes32 currentHash = manager.getKeyRingHash(account);
        vm.expectRevert(abi.encodeWithSelector(KeyRingUnchanged.selector, currentHash));
        manager.setKeyRing(account, currentHash, _sessions1(), ACCEPT_SIG);
    }

    function test_setKeyRing_revertsOnFailedAdminAuthorization() public {
        address account = _createDefault(bytes32(uint256(15)));
        bytes32 currentHash = manager.getKeyRingHash(account);
        vm.expectRevert(AdminAuthorizationFailed.selector);
        manager.setKeyRing(account, currentHash, _sessions2(), REJECT_SIG);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                VIEWS                                    ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_views_revertOnMissingAccount() public {
        vm.expectRevert(abi.encodeWithSelector(AccountDoesNotExist.selector, address(0xabc)));
        manager.getAdmin(address(0xabc));
        vm.expectRevert(abi.encodeWithSelector(AccountDoesNotExist.selector, address(0xabc)));
        manager.getAdminNonce(address(0xabc));
        vm.expectRevert(abi.encodeWithSelector(AccountDoesNotExist.selector, address(0xabc)));
        manager.getKeyRingHash(address(0xabc));
    }

    function test_getAuthorizedSessionVerifier_revertsWhenAbsent() public {
        address account = _createDefault(bytes32(uint256(16)));
        vm.expectRevert(abi.encodeWithSelector(UnauthorizedSessionVerifier.selector, address(0xbabe)));
        manager.getAuthorizedSessionVerifier(account, address(0xbabe));
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                       HASH DOMAIN SEPARATION                            ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_hashes_createAndSetKeyRingAreDifferent() public view {
        IWorldChainAccountManager.WorldChainAccountVerifier memory admin = _admin(hex"");
        bytes32 adminHash_ = WorldChainAccountHashes.adminHash(admin);
        bytes32 keyRingHash_ = WorldChainAccountHashes.keyRingHash(_sessions1());
        address account = address(0x1234);
        bytes32 c = WorldChainAccountHashes.createHash(
            1, address(this), bytes32(uint256(1)), account, adminHash_, bytes32(0), keyRingHash_
        );
        bytes32 s = WorldChainAccountHashes.setKeyRingHash(1, address(this), account, 0, keyRingHash_, keyRingHash_);
        assertTrue(c != s, "create and setKeyRing hashes must differ");
    }

    function test_hashes_chainIdBindingBreaksReplay() public view {
        IWorldChainAccountManager.WorldChainAccountVerifier memory admin = _admin(hex"");
        bytes32 adminHash_ = WorldChainAccountHashes.adminHash(admin);
        bytes32 keyRingHash_ = WorldChainAccountHashes.keyRingHash(_sessions1());
        bytes32 routerCodeHash = manager.getAccountRouterCodeHash();
        address account = address(0x9876);
        bytes32 a = WorldChainAccountHashes.createHash(
            1, address(manager), routerCodeHash, account, adminHash_, bytes32(0), keyRingHash_
        );
        bytes32 b = WorldChainAccountHashes.createHash(
            2, address(manager), routerCodeHash, account, adminHash_, bytes32(0), keyRingHash_
        );
        assertTrue(a != b, "createHash must bind chainId");
    }

    function test_hashes_nonceBindingBreaksReplay() public view {
        bytes32 keyRingHash_ = WorldChainAccountHashes.keyRingHash(_sessions1());
        address account = address(0x5555);
        bytes32 h0 = WorldChainAccountHashes.setKeyRingHash(1, address(manager), account, 0, keyRingHash_, keyRingHash_);
        bytes32 h1 = WorldChainAccountHashes.setKeyRingHash(1, address(manager), account, 1, keyRingHash_, keyRingHash_);
        assertTrue(h0 != h1, "setKeyRingHash must bind adminNonce");
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                        ACTIVATION PARAMETERS                            ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_activationParams_ownerMayUpdateUntilFrozen() public {
        vm.prank(OWNER);
        manager.setEip1271ValidationGasLimit(123_000);
        assertEq(manager.eip1271ValidationGasLimit(), 123_000);
    }

    function test_activationParams_freezeBlocksFurtherUpdates() public {
        vm.startPrank(OWNER);
        manager.freezeActivationParameters();
        vm.expectRevert(ActivationParametersAlreadyFrozen.selector);
        manager.setEip1271ValidationGasLimit(999);
        vm.expectRevert(ActivationParametersAlreadyFrozen.selector);
        manager.freezeActivationParameters();
        vm.stopPrank();
        assertTrue(manager.activationParametersFrozen());
    }

    function test_initialize_rejectsZeroOwner() public {
        WorldChainAccountManagerImplV1 impl = new WorldChainAccountManagerImplV1();
        bytes memory initCall = abi.encodeCall(WorldChainAccountManagerImplV1.initialize, (address(0)));
        vm.expectRevert(AddressZero.selector);
        new WorldChainAccountManager(address(impl), initCall);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              ROUTER GATING                              ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_router_installAdminRejectsNonManager() public {
        address account = _createDefault(bytes32(uint256(20)));
        address router = manager.getAccountRouter(account);
        IWorldChainAccountManager.WorldChainAccountVerifier memory admin = _admin(hex"");
        vm.expectRevert(ManagerOnly.selector);
        IWorldChainAccountRouter(router).installAdmin(admin);
    }

    function test_router_installKeyRingRejectsNonManager() public {
        address account = _createDefault(bytes32(uint256(21)));
        address router = manager.getAccountRouter(account);
        vm.expectRevert(ManagerOnly.selector);
        IWorldChainAccountRouter(router).installKeyRing(_sessions2());
    }

    function test_router_unauthorizedSessionVerifierReturnsFailureValue() public {
        address account = _createDefault(bytes32(uint256(22)));
        address router = manager.getAccountRouter(account);
        bytes4 v =
            IWorldChainAccountRouter(router).isValidSignatureForVerifier(address(0xdead), keccak256("h"), ACCEPT_SIG);
        assertEq(v, WorldChainAccountConstants.EIP1271_FAILURE_VALUE);
    }
}
