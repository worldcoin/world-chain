// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {IERC165} from "@openzeppelin/contracts/utils/introspection/IERC165.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";

import {SubsidyAccounting} from "../src/SubsidyAccounting.sol";
import {SubsidyAccountingImplV1} from "../src/SubsidyAccountingImplV1.sol";
import {WorldChainRpSigner} from "../src/WorldChainRpSigner.sol";
import {WorldChainRpSignerImplV1} from "../src/WorldChainRpSignerImplV1.sol";
import {IActionVerifier} from "../src/interfaces/IActionVerifier.sol";
import {IRpSigner} from "../src/interfaces/IRpSigner.sol";
import {IWorldIDVerifier} from "../src/interfaces/IWorldIDVerifier.sol";
import {MockActionVerifier} from "./mocks/MockActionVerifier.sol";
import {MockWorldIDVerifier} from "./mocks/MockWorldIDVerifier.sol";

/// @title WorldChainRpSignerImplV1 tests
contract WorldChainRpSignerImplV1Test is Test {
    WorldChainRpSignerImplV1 internal signer;
    WorldChainRpSignerImplV1 internal signerImpl;
    SubsidyAccountingImplV1 internal subsidy;
    MockWorldIDVerifier internal worldIDVerifier;

    address internal constant OWNER = address(0xC0FFEE);
    address internal constant ATTACKER = address(0xBAD);

    bytes4 internal constant MAGICVALUE = 0x35dbc8de;
    uint8 internal constant EXPECTED_VERSION = 1;
    uint64 internal constant MAX_REQUEST_TTL = 1 hours;
    uint64 internal constant PERIOD_LENGTH = 30 days;

    uint256 internal constant CODE_BAD_VERSION = 100;
    uint256 internal constant CODE_EXPIRED = 101;
    uint256 internal constant CODE_NOT_YET_VALID = 102;
    uint256 internal constant CODE_TTL_TOO_LONG = 103;
    uint256 internal constant CODE_BAD_TIMESTAMP_ORDER = 104;
    uint256 internal constant CODE_NON_EMPTY_DATA = 105;
    uint256 internal constant CODE_BAD_UNIQUENESS_ACTION = 201;

    event RpSignerImplInitialized(address indexed owner);
    event ActionVerifierAdded(IActionVerifier indexed verifier);
    event ActionVerifierRemoved(IActionVerifier indexed verifier);

    function setUp() public {
        vm.warp(60 days);

        worldIDVerifier = new MockWorldIDVerifier(true);
        SubsidyAccountingImplV1 subsidyImpl = new SubsidyAccountingImplV1();
        bytes memory subsidyInit = abi.encodeCall(SubsidyAccountingImplV1.initialize, (worldIDVerifier, OWNER));
        subsidy = SubsidyAccountingImplV1(address(new SubsidyAccounting(address(subsidyImpl), subsidyInit)));

        signerImpl = new WorldChainRpSignerImplV1();
        IActionVerifier[] memory initialVerifiers = new IActionVerifier[](1);
        initialVerifiers[0] = IActionVerifier(address(subsidy));
        bytes memory signerInit = abi.encodeCall(WorldChainRpSignerImplV1.initialize, (initialVerifiers, OWNER));
        signer = WorldChainRpSignerImplV1(address(new WorldChainRpSigner(address(signerImpl), signerInit)));
    }

    function _wellFormed() internal view returns (uint8, uint256, uint64, uint64, bytes memory) {
        return (EXPECTED_VERSION, 0, uint64(block.timestamp - 1 minutes), uint64(block.timestamp + 5 minutes), "");
    }

    function _validUniquenessAction() internal view returns (uint256) {
        return subsidy.actionForPeriod(uint64(block.timestamp / PERIOD_LENGTH));
    }

    function _sessionAction() internal pure returns (uint256) {
        return uint256(0x02) << 240 | 0x1234;
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              ERC-165                                    ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_supportsInterface_irpSigner() public view {
        assertTrue(signer.supportsInterface(type(IRpSigner).interfaceId));
    }

    function test_supportsInterface_ierc165() public view {
        assertTrue(signer.supportsInterface(type(IERC165).interfaceId));
    }

    function test_supportsInterface_unknown_isFalse() public view {
        assertFalse(signer.supportsInterface(bytes4(0xdeadbeef)));
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                       SESSION (class 0x02)                              ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_verify_session_returnsMagic_unconditionally() public view {
        (uint8 v, uint256 n, uint64 c, uint64 e, bytes memory d) = _wellFormed();
        assertEq(signer.verifyRpRequest(v, n, c, e, _sessionAction(), d), MAGICVALUE);
    }

    function testFuzz_verify_session_acceptsRandomLowBits(uint256 lowBits) public view {
        (uint8 v, uint256 n, uint64 c, uint64 e, bytes memory d) = _wellFormed();
        uint256 action = (uint256(0x02) << 240) | (lowBits & ((uint256(1) << 240) - 1));
        assertEq(signer.verifyRpRequest(v, n, c, e, action, d), MAGICVALUE);
    }

    function test_verify_session_ignoresVerifierList() public {
        // Empty verifier list still accepts Session class.
        vm.prank(OWNER);
        signer.removeVerifier(IActionVerifier(address(subsidy)));
        (uint8 v, uint256 n, uint64 c, uint64 e, bytes memory d) = _wellFormed();
        assertEq(signer.verifyRpRequest(v, n, c, e, _sessionAction(), d), MAGICVALUE);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                         CLASS / ENVELOPE                                ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_verify_nonSessionClass_routesToVerifiers() public {
        // Non-Session classes (including 0x01, future protocol additions, etc.) flow to
        // verifier iteration — no class-prefix gate at the signer. A random class-0x01
        // action no verifier matches reverts BAD_UNIQUENESS_ACTION.
        (uint8 v, uint256 n, uint64 c, uint64 e, bytes memory d) = _wellFormed();
        uint256 action = uint256(0x01) << 240;
        vm.expectRevert(abi.encodeWithSelector(IRpSigner.RpInvalidRequest.selector, CODE_BAD_UNIQUENESS_ACTION));
        signer.verifyRpRequest(v, n, c, e, action, d);
    }

    function test_verify_rejects_badVersion_zero() public {
        (, uint256 n, uint64 c, uint64 e, bytes memory d) = _wellFormed();
        vm.expectRevert(abi.encodeWithSelector(IRpSigner.RpInvalidRequest.selector, CODE_BAD_VERSION));
        signer.verifyRpRequest(0, n, c, e, _sessionAction(), d);
    }

    function test_verify_rejects_badVersion_two() public {
        (, uint256 n, uint64 c, uint64 e, bytes memory d) = _wellFormed();
        vm.expectRevert(abi.encodeWithSelector(IRpSigner.RpInvalidRequest.selector, CODE_BAD_VERSION));
        signer.verifyRpRequest(2, n, c, e, _sessionAction(), d);
    }

    function test_verify_rejects_nonEmptyData() public {
        (uint8 v, uint256 n, uint64 c, uint64 e,) = _wellFormed();
        vm.expectRevert(abi.encodeWithSelector(IRpSigner.RpInvalidRequest.selector, CODE_NON_EMPTY_DATA));
        signer.verifyRpRequest(v, n, c, e, _sessionAction(), hex"00");
    }

    function test_verify_rejects_expired() public {
        (uint8 v, uint256 n,,, bytes memory d) = _wellFormed();
        uint64 c = uint64(block.timestamp - 5 minutes);
        uint64 e = uint64(block.timestamp - 1);
        vm.expectRevert(abi.encodeWithSelector(IRpSigner.RpInvalidRequest.selector, CODE_EXPIRED));
        signer.verifyRpRequest(v, n, c, e, _sessionAction(), d);
    }

    function test_verify_rejects_notYetValid() public {
        (uint8 v, uint256 n,,, bytes memory d) = _wellFormed();
        uint64 c = uint64(block.timestamp + 1);
        uint64 e = uint64(block.timestamp + 10 minutes);
        vm.expectRevert(abi.encodeWithSelector(IRpSigner.RpInvalidRequest.selector, CODE_NOT_YET_VALID));
        signer.verifyRpRequest(v, n, c, e, _sessionAction(), d);
    }

    function test_verify_rejects_ttlTooLong() public {
        (uint8 v, uint256 n,,, bytes memory d) = _wellFormed();
        uint64 c = uint64(block.timestamp);
        uint64 e = c + MAX_REQUEST_TTL + 1;
        vm.expectRevert(abi.encodeWithSelector(IRpSigner.RpInvalidRequest.selector, CODE_TTL_TOO_LONG));
        signer.verifyRpRequest(v, n, c, e, _sessionAction(), d);
    }

    function test_verify_rejects_badTimestampOrder() public {
        (uint8 v, uint256 n,,, bytes memory d) = _wellFormed();
        uint64 c = uint64(block.timestamp + 10);
        uint64 e = uint64(block.timestamp + 5);
        vm.expectRevert(abi.encodeWithSelector(IRpSigner.RpInvalidRequest.selector, CODE_BAD_TIMESTAMP_ORDER));
        signer.verifyRpRequest(v, n, c, e, _sessionAction(), d);
    }

    function test_verify_acceptsTtlExactly_oneHour() public view {
        uint64 c = uint64(block.timestamp);
        uint64 e = c + MAX_REQUEST_TTL;
        assertEq(signer.verifyRpRequest(EXPECTED_VERSION, 0, c, e, _sessionAction(), ""), MAGICVALUE);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                   UNIQUENESS — verifier routing                         ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_verify_uniqueness_singleVerifier_accepts() public view {
        (uint8 v, uint256 n, uint64 c, uint64 e, bytes memory d) = _wellFormed();
        assertEq(signer.verifyRpRequest(v, n, c, e, _validUniquenessAction(), d), MAGICVALUE);
    }

    function test_verify_uniqueness_singleVerifier_rejects() public {
        (uint8 v, uint256 n, uint64 c, uint64 e, bytes memory d) = _wellFormed();
        vm.expectRevert(abi.encodeWithSelector(IRpSigner.RpInvalidRequest.selector, CODE_BAD_UNIQUENESS_ACTION));
        signer.verifyRpRequest(v, n, c, e, 0xBEEF, d);
    }

    function test_verify_uniqueness_multipleVerifiers_anyAccepts() public {
        MockActionVerifier mock = new MockActionVerifier(false);
        uint256 mockAction = 0xC0FFEE; // top byte 0 — class Uniqueness
        mock.setAllowedAction(mockAction);

        vm.prank(OWNER);
        signer.addVerifier(IActionVerifier(address(mock)));

        (uint8 v, uint256 n, uint64 c, uint64 e, bytes memory d) = _wellFormed();
        assertEq(signer.verifyRpRequest(v, n, c, e, mockAction, d), MAGICVALUE);
    }

    function test_verify_uniqueness_allVerifiersReject_reverts() public {
        MockActionVerifier mock = new MockActionVerifier(false);
        vm.prank(OWNER);
        signer.addVerifier(IActionVerifier(address(mock)));

        (uint8 v, uint256 n, uint64 c, uint64 e, bytes memory d) = _wellFormed();
        vm.expectRevert(abi.encodeWithSelector(IRpSigner.RpInvalidRequest.selector, CODE_BAD_UNIQUENESS_ACTION));
        signer.verifyRpRequest(v, n, c, e, 0x123456, d);
    }

    function test_verify_uniqueness_emptyVerifierList_reverts() public {
        vm.prank(OWNER);
        signer.removeVerifier(IActionVerifier(address(subsidy)));

        // Use a deterministically non-Session action (top byte 0x00) so the Session
        // fast-route doesn't fire — we need the empty-verifier iteration path.
        uint256 nonSessionAction = 0x1234;
        (uint8 v, uint256 n, uint64 c, uint64 e, bytes memory d) = _wellFormed();
        vm.expectRevert(abi.encodeWithSelector(IRpSigner.RpInvalidRequest.selector, CODE_BAD_UNIQUENESS_ACTION));
        signer.verifyRpRequest(v, n, c, e, nonSessionAction, d);
    }

    function test_verify_uniqueness_invalidatedByVerifierSwap() public {
        (uint8 v, uint256 n, uint64 c, uint64 e, bytes memory d) = _wellFormed();
        uint256 actionBefore = _validUniquenessAction();
        assertEq(signer.verifyRpRequest(v, n, c, e, actionBefore, d), MAGICVALUE);

        IWorldIDVerifier newVerifier = IWorldIDVerifier(address(new MockWorldIDVerifier(true)));
        vm.prank(OWNER);
        subsidy.setWorldIDVerifier(newVerifier);

        vm.expectRevert(abi.encodeWithSelector(IRpSigner.RpInvalidRequest.selector, CODE_BAD_UNIQUENESS_ACTION));
        signer.verifyRpRequest(v, n, c, e, actionBefore, d);

        assertEq(signer.verifyRpRequest(v, n, c, e, _validUniquenessAction(), d), MAGICVALUE);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              ADMIN                                      ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_addVerifier_byOwner_emitsAndStores() public {
        MockActionVerifier mock = new MockActionVerifier(true);

        vm.expectEmit(true, true, true, true, address(signer));
        emit ActionVerifierAdded(IActionVerifier(address(mock)));
        vm.prank(OWNER);
        signer.addVerifier(IActionVerifier(address(mock)));

        IActionVerifier[] memory list = signer.getVerifiers();
        assertEq(list.length, 2, "two verifiers registered");
        assertEq(address(list[1]), address(mock), "mock appended");
    }

    function test_addVerifier_revertIf_notOwner() public {
        MockActionVerifier mock = new MockActionVerifier(true);
        vm.prank(ATTACKER);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, ATTACKER));
        signer.addVerifier(IActionVerifier(address(mock)));
    }

    function test_addVerifier_revertIf_zero() public {
        vm.prank(OWNER);
        vm.expectRevert(WorldChainRpSignerImplV1.AddressZero.selector);
        signer.addVerifier(IActionVerifier(address(0)));
    }

    function test_addVerifier_revertIf_duplicate() public {
        vm.prank(OWNER);
        vm.expectRevert(
            abi.encodeWithSelector(
                WorldChainRpSignerImplV1.VerifierAlreadyRegistered.selector, IActionVerifier(address(subsidy))
            )
        );
        signer.addVerifier(IActionVerifier(address(subsidy)));
    }

    function test_removeVerifier_byOwner_emitsAndShrinks() public {
        vm.expectEmit(true, true, true, true, address(signer));
        emit ActionVerifierRemoved(IActionVerifier(address(subsidy)));
        vm.prank(OWNER);
        signer.removeVerifier(IActionVerifier(address(subsidy)));

        assertEq(signer.getVerifiers().length, 0, "list emptied");
    }

    function test_removeVerifier_revertIf_notOwner() public {
        vm.prank(ATTACKER);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, ATTACKER));
        signer.removeVerifier(IActionVerifier(address(subsidy)));
    }

    function test_removeVerifier_revertIf_notFound() public {
        MockActionVerifier mock = new MockActionVerifier(true);
        vm.prank(OWNER);
        vm.expectRevert(
            abi.encodeWithSelector(WorldChainRpSignerImplV1.VerifierNotFound.selector, IActionVerifier(address(mock)))
        );
        signer.removeVerifier(IActionVerifier(address(mock)));
    }

    function test_getVerifiers_returnsList() public {
        MockActionVerifier mock = new MockActionVerifier(true);
        vm.prank(OWNER);
        signer.addVerifier(IActionVerifier(address(mock)));

        IActionVerifier[] memory list = signer.getVerifiers();
        assertEq(list.length, 2);
        assertEq(address(list[0]), address(subsidy));
        assertEq(address(list[1]), address(mock));
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                            INITIALIZE                                   ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_initialize_storesVerifiers_emitsEvent() public {
        WorldChainRpSignerImplV1 freshImpl = new WorldChainRpSignerImplV1();
        IActionVerifier[] memory initial = new IActionVerifier[](1);
        initial[0] = IActionVerifier(address(subsidy));

        bytes memory init = abi.encodeCall(WorldChainRpSignerImplV1.initialize, (initial, OWNER));
        WorldChainRpSignerImplV1 fresh =
            WorldChainRpSignerImplV1(address(new WorldChainRpSigner(address(freshImpl), init)));

        assertEq(fresh.getVerifiers().length, 1);
        assertEq(fresh.owner(), OWNER);
    }

    function test_initialize_emptyVerifierList_allowed() public {
        WorldChainRpSignerImplV1 freshImpl = new WorldChainRpSignerImplV1();
        IActionVerifier[] memory initial = new IActionVerifier[](0);
        bytes memory init = abi.encodeCall(WorldChainRpSignerImplV1.initialize, (initial, OWNER));
        WorldChainRpSignerImplV1 fresh =
            WorldChainRpSignerImplV1(address(new WorldChainRpSigner(address(freshImpl), init)));

        assertEq(fresh.getVerifiers().length, 0);
    }

    function test_initialize_revertIf_zeroVerifierInList() public {
        WorldChainRpSignerImplV1 freshImpl = new WorldChainRpSignerImplV1();
        IActionVerifier[] memory initial = new IActionVerifier[](1);
        initial[0] = IActionVerifier(address(0));
        bytes memory init = abi.encodeCall(WorldChainRpSignerImplV1.initialize, (initial, OWNER));
        vm.expectRevert();
        new WorldChainRpSigner(address(freshImpl), init);
    }

    function test_initialize_revertIf_duplicateVerifierInList() public {
        WorldChainRpSignerImplV1 freshImpl = new WorldChainRpSignerImplV1();
        IActionVerifier[] memory initial = new IActionVerifier[](2);
        initial[0] = IActionVerifier(address(subsidy));
        initial[1] = IActionVerifier(address(subsidy));
        bytes memory init = abi.encodeCall(WorldChainRpSignerImplV1.initialize, (initial, OWNER));
        vm.expectRevert();
        new WorldChainRpSigner(address(freshImpl), init);
    }

    function test_initialize_revertIf_zeroOwner() public {
        WorldChainRpSignerImplV1 freshImpl = new WorldChainRpSignerImplV1();
        IActionVerifier[] memory initial = new IActionVerifier[](0);
        bytes memory init = abi.encodeCall(WorldChainRpSignerImplV1.initialize, (initial, address(0)));
        vm.expectRevert();
        new WorldChainRpSigner(address(freshImpl), init);
    }

    function test_initialize_revertIf_calledTwice() public {
        IActionVerifier[] memory initial = new IActionVerifier[](0);
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        signer.initialize(initial, OWNER);
    }

    function test_initialize_revertIf_calledOnImplementation() public {
        IActionVerifier[] memory initial = new IActionVerifier[](0);
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        signerImpl.initialize(initial, OWNER);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                            UUPS UPGRADE                                 ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_uupsUpgrade_preservesState() public {
        WorldChainRpSignerImplV1 newImpl = new WorldChainRpSignerImplV1();
        vm.prank(OWNER);
        signer.upgradeToAndCall(address(newImpl), bytes(""));

        assertEq(signer.getVerifiers().length, 1, "verifier list preserved");
        assertEq(address(signer.getVerifiers()[0]), address(subsidy), "verifier identity preserved");
    }

    function test_uupsUpgrade_onlyOwner() public {
        WorldChainRpSignerImplV1 newImpl = new WorldChainRpSignerImplV1();
        vm.prank(ATTACKER);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, ATTACKER));
        signer.upgradeToAndCall(address(newImpl), bytes(""));
    }
}
