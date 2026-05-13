// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {Ownable2StepUpgradeable} from "@openzeppelin/contracts-upgradeable/access/Ownable2StepUpgradeable.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";

import {SubsidyAccounting} from "../src/SubsidyAccounting.sol";
import {SubsidyAccountingImplV1} from "../src/SubsidyAccountingImplV1.sol";
import {ISubsidyAccounting} from "../src/interfaces/ISubsidyAccounting.sol";
import {IWorldIDVerifier} from "../src/interfaces/IWorldIDVerifier.sol";
import {MockWorldIDVerifier} from "./mocks/MockWorldIDVerifier.sol";

/// @title SubsidyAccountingImplV1 skeleton tests
/// @notice Init, admin, ownership, view-defaults, and stub-revert coverage for the WIP-1002
///         skeleton entry. Body coverage for `claimSubsidy`, session-proof methods, and
///         `consumeBudget` lands in subsequent entries on the same PR.
contract SubsidyAccountingImplV1Test is Test {
    SubsidyAccountingImplV1 internal subsidy;
    SubsidyAccountingImplV1 internal impl;
    MockWorldIDVerifier internal verifier;

    address internal constant OWNER = address(0xC0FFEE);
    address internal constant ATTACKER = address(0xBAD);

    event SubsidyAccountingImplInitialized(IWorldIDVerifier indexed worldIDVerifier, address indexed owner);
    event WorldIDVerifierSet(address indexed worldIDVerifier);
    event CredentialBudgetSet(uint64 indexed issuerSchemaId, uint256 budgetWei);
    event SubsidyClaimed(
        uint256 indexed nullifier, uint256 indexed sessionId, uint64 periodNumber, uint256 totalBudgetWei
    );

    uint64 internal constant WORLD_CHAIN_RP_ID = 480;
    uint64 internal constant PERIOD_LENGTH = 30 days;
    uint256 internal constant PROOF_NONCE = 0;
    uint256 internal constant CREDENTIAL_GENESIS_ISSUED_AT_MIN = 0;
    bytes32 internal constant CLAIM_SUBSIDY_TAG = "CLAIM_SUBSIDY";

    function setUp() public {
        // Move beyond the first period so currentPeriod() > 0 and unset records (period 0)
        // can be reliably distinguished from current ones.
        vm.warp(60 days);

        verifier = new MockWorldIDVerifier(true);
        impl = new SubsidyAccountingImplV1();
        bytes memory initCall = abi.encodeCall(SubsidyAccountingImplV1.initialize, (verifier, OWNER));
        subsidy = SubsidyAccountingImplV1(address(new SubsidyAccounting(address(impl), initCall)));
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              INITIALIZE                                 ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_initialize_setsState_andEmitsEvent() public {
        // Re-deploy in this test so we can assert the init event fires.
        SubsidyAccountingImplV1 freshImpl = new SubsidyAccountingImplV1();
        bytes memory initCall = abi.encodeCall(SubsidyAccountingImplV1.initialize, (verifier, OWNER));

        vm.expectEmit(true, true, true, true);
        emit SubsidyAccountingImplInitialized(verifier, OWNER);
        SubsidyAccountingImplV1 fresh =
            SubsidyAccountingImplV1(address(new SubsidyAccounting(address(freshImpl), initCall)));

        assertEq(address(fresh.worldIDVerifier()), address(verifier), "verifier persisted");
        assertEq(fresh.owner(), OWNER, "owner persisted");
    }

    function test_initialize_revertIf_zeroVerifier() public {
        SubsidyAccountingImplV1 freshImpl = new SubsidyAccountingImplV1();
        bytes memory initCall =
            abi.encodeCall(SubsidyAccountingImplV1.initialize, (IWorldIDVerifier(address(0)), OWNER));
        vm.expectRevert(ISubsidyAccounting.AddressZero.selector);
        new SubsidyAccounting(address(freshImpl), initCall);
    }

    function test_initialize_revertIf_zeroOwner() public {
        SubsidyAccountingImplV1 freshImpl = new SubsidyAccountingImplV1();
        bytes memory initCall = abi.encodeCall(SubsidyAccountingImplV1.initialize, (verifier, address(0)));
        vm.expectRevert(ISubsidyAccounting.AddressZero.selector);
        new SubsidyAccounting(address(freshImpl), initCall);
    }

    function test_initialize_revertIf_calledTwice() public {
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        subsidy.initialize(verifier, OWNER);
    }

    function test_initialize_revertIf_calledOnImplementation() public {
        // The impl had `_disableInitializers()` in its constructor.
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        impl.initialize(verifier, OWNER);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                          ADMIN: setWorldIDVerifier                      ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_setWorldIDVerifier_byOwner_persists_andEmits() public {
        IWorldIDVerifier newVerifier = IWorldIDVerifier(address(new MockWorldIDVerifier(true)));

        vm.expectEmit(true, true, true, true, address(subsidy));
        emit WorldIDVerifierSet(address(newVerifier));

        vm.prank(OWNER);
        subsidy.setWorldIDVerifier(newVerifier);

        assertEq(address(subsidy.worldIDVerifier()), address(newVerifier));
    }

    function test_setWorldIDVerifier_revertIf_notOwner() public {
        IWorldIDVerifier newVerifier = IWorldIDVerifier(address(new MockWorldIDVerifier(true)));
        vm.prank(ATTACKER);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, ATTACKER));
        subsidy.setWorldIDVerifier(newVerifier);
    }

    function test_setWorldIDVerifier_revertIf_zero() public {
        vm.prank(OWNER);
        vm.expectRevert(ISubsidyAccounting.AddressZero.selector);
        subsidy.setWorldIDVerifier(IWorldIDVerifier(address(0)));
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                          ADMIN: setCredentialBudget                     ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_setCredentialBudget_byOwner_persists_andEmits() public {
        uint64 schemaId = 7;
        uint256 budgetWei = 1 ether;

        vm.expectEmit(true, true, true, true, address(subsidy));
        emit CredentialBudgetSet(schemaId, budgetWei);

        vm.prank(OWNER);
        subsidy.setCredentialBudget(schemaId, budgetWei);

        assertEq(subsidy.credentialBudget(schemaId), budgetWei);
    }

    function test_setCredentialBudget_zeroBudgetAllowed() public {
        vm.prank(OWNER);
        subsidy.setCredentialBudget(7, 1 ether);
        vm.prank(OWNER);
        subsidy.setCredentialBudget(7, 0);
        assertEq(subsidy.credentialBudget(7), 0, "budget can be zeroed");
    }

    function test_setCredentialBudget_revertIf_notOwner() public {
        vm.prank(ATTACKER);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, ATTACKER));
        subsidy.setCredentialBudget(7, 1 ether);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                          OWNERSHIP TRANSFER                             ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_ownershipTransfer_twoStep() public {
        address NEW_OWNER = address(0xABCD);

        vm.prank(OWNER);
        subsidy.transferOwnership(NEW_OWNER);
        assertEq(subsidy.owner(), OWNER, "owner unchanged before accept");

        vm.prank(NEW_OWNER);
        subsidy.acceptOwnership();
        assertEq(subsidy.owner(), NEW_OWNER, "owner updated after accept");
    }

    function test_acceptOwnership_revertIf_notPendingOwner() public {
        vm.prank(OWNER);
        subsidy.transferOwnership(address(0xABCD));

        vm.prank(ATTACKER);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, ATTACKER));
        subsidy.acceptOwnership();
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                            VIEW DEFAULTS                                ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_views_emptyState() public view {
        uint256 nullifier = 0xDEADBEEF;
        address account = address(0x1234);

        assertEq(subsidy.getBudget(nullifier), 0, "no record => 0 budget by nullifier");
        assertEq(subsidy.getBudget(account), 0, "no records => 0 budget by account");
        assertFalse(subsidy.isAuthorized(account, nullifier), "empty authorized set");
        assertEq(subsidy.getNullifiers(account).length, 0, "empty reverse index");
        assertFalse(subsidy.isClaimed(nullifier, 1), "no claimed credentials");
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                       ENTRY-POINT STUBS (NotImplemented)                ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_claimAdditionalCredential_revertsRecordDoesNotExist_whenAbsent() public {
        uint256[5] memory proof;
        vm.expectRevert(ISubsidyAccounting.RecordDoesNotExist.selector);
        subsidy.claimAdditionalCredential(0xBEEF, 1, 0, 0, proof);
    }

    function test_setAuthorized_revertsRecordDoesNotExist_whenAbsent() public {
        address[] memory empty = new address[](0);
        uint256[5] memory proof;
        vm.expectRevert(ISubsidyAccounting.RecordDoesNotExist.selector);
        subsidy.setAuthorized(0xDEAD, 0, empty, 0, 0, proof);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              CLAIM SUBSIDY                              ///
    ///////////////////////////////////////////////////////////////////////////////

    address internal constant ALICE = address(0xA11CE);
    address internal constant BOB = address(0xB0B);
    uint64 internal constant SCHEMA_POH = 1;
    uint64 internal constant SCHEMA_PHONE = 2;
    uint64 internal constant SCHEMA_NFC = 3;

    function _proof(uint256 seed) internal pure returns (uint256[5] memory p) {
        p[0] = seed;
        p[1] = seed + 1;
        p[2] = seed + 2;
        p[3] = seed + 3;
        p[4] = seed + 4;
    }

    function _signalHash(uint256 sessionId, address[] memory addrs, address sender) internal pure returns (uint256) {
        SubsidyAccountingImplV1.ClaimSubsidySignal memory s = SubsidyAccountingImplV1.ClaimSubsidySignal({
            tag: CLAIM_SUBSIDY_TAG, sessionId: sessionId, addAddresses: addrs, msgSender: sender
        });
        return uint256(keccak256(abi.encode(s))) >> 8;
    }

    function _action(uint64 period) internal pure returns (uint256) {
        // `_registrationVersion` is 0 at deploy and the verifier-swap tests don't run claim
        // paths after a swap, so 0 is the correct mixer value for proof-action expectations.
        uint64 registrationVersion = 0;
        return uint256(keccak256(abi.encodePacked("period_proof", period, registrationVersion))) >> 8;
    }

    function _addrs1(address a) internal pure returns (address[] memory out) {
        out = new address[](1);
        out[0] = a;
    }

    function _addrs2(address a, address b) internal pure returns (address[] memory out) {
        out = new address[](2);
        out[0] = a;
        out[1] = b;
    }

    function _items1(uint64 schemaId, uint256 seed) internal pure returns (ISubsidyAccounting.ClaimItem[] memory out) {
        out = new ISubsidyAccounting.ClaimItem[](1);
        out[0] = ISubsidyAccounting.ClaimItem({issuerSchemaId: schemaId, proof: _proof(seed)});
    }

    function _items2(uint64 schemaA, uint64 schemaB, uint256 seedA, uint256 seedB)
        internal
        pure
        returns (ISubsidyAccounting.ClaimItem[] memory out)
    {
        out = new ISubsidyAccounting.ClaimItem[](2);
        out[0] = ISubsidyAccounting.ClaimItem({issuerSchemaId: schemaA, proof: _proof(seedA)});
        out[1] = ISubsidyAccounting.ClaimItem({issuerSchemaId: schemaB, proof: _proof(seedB)});
    }

    function _setBudgets() internal {
        vm.startPrank(OWNER);
        subsidy.setCredentialBudget(SCHEMA_POH, 50_000 gwei);
        subsidy.setCredentialBudget(SCHEMA_PHONE, 10_000 gwei);
        subsidy.setCredentialBudget(SCHEMA_NFC, 20_000 gwei);
        vm.stopPrank();
    }

    function test_claimSubsidy_happyPath_singleCredential_emitsAndStores() public {
        _setBudgets();
        uint256 nullifier = 0xA001;
        uint256 sessionId = 0x5E5510;
        address[] memory addrs = _addrs1(ALICE);
        ISubsidyAccounting.ClaimItem[] memory items = _items1(SCHEMA_POH, 1000);
        uint64 period = uint64(block.timestamp / PERIOD_LENGTH);
        uint64 expiresAtMin = period * PERIOD_LENGTH;
        uint256 sigHash = _signalHash(sessionId, addrs, address(this));

        vm.expectCall(
            address(verifier),
            abi.encodeCall(
                IWorldIDVerifier.verify,
                (
                    nullifier,
                    _action(period),
                    WORLD_CHAIN_RP_ID,
                    PROOF_NONCE,
                    sigHash,
                    expiresAtMin,
                    SCHEMA_POH,
                    CREDENTIAL_GENESIS_ISSUED_AT_MIN,
                    _proof(1000)
                )
            )
        );

        vm.expectEmit(true, true, true, true, address(subsidy));
        emit SubsidyClaimed(nullifier, sessionId, period, 50_000 gwei);

        subsidy.claimSubsidy(nullifier, sessionId, addrs, items);

        assertEq(subsidy.getBudget(nullifier), 50_000 gwei, "budget stored");
        assertTrue(subsidy.isAuthorized(ALICE, nullifier), "alice authorized");
        assertTrue(subsidy.isClaimed(nullifier, SCHEMA_POH), "schema marked claimed");
        uint256[] memory ns = subsidy.getNullifiers(ALICE);
        assertEq(ns.length, 1);
        assertEq(ns[0], nullifier);
        assertEq(subsidy.getBudget(ALICE), 50_000 gwei, "address-side budget matches");
    }

    function test_claimSubsidy_happyPath_multiCredential_summedBudget() public {
        _setBudgets();
        uint256 nullifier = 0xA002;
        uint256 sessionId = 0xABC;
        address[] memory addrs = _addrs2(ALICE, BOB);
        ISubsidyAccounting.ClaimItem[] memory items = _items2(SCHEMA_POH, SCHEMA_PHONE, 1, 2);
        uint64 period = uint64(block.timestamp / PERIOD_LENGTH);
        uint64 expiresAtMin = period * PERIOD_LENGTH;
        uint256 sigHash = _signalHash(sessionId, addrs, address(this));

        vm.expectCall(
            address(verifier),
            abi.encodeCall(
                IWorldIDVerifier.verify,
                (
                    nullifier,
                    _action(period),
                    WORLD_CHAIN_RP_ID,
                    PROOF_NONCE,
                    sigHash,
                    expiresAtMin,
                    SCHEMA_POH,
                    CREDENTIAL_GENESIS_ISSUED_AT_MIN,
                    _proof(1)
                )
            )
        );
        vm.expectCall(
            address(verifier),
            abi.encodeCall(
                IWorldIDVerifier.verify,
                (
                    nullifier,
                    _action(period),
                    WORLD_CHAIN_RP_ID,
                    PROOF_NONCE,
                    sigHash,
                    expiresAtMin,
                    SCHEMA_PHONE,
                    CREDENTIAL_GENESIS_ISSUED_AT_MIN,
                    _proof(2)
                )
            )
        );

        vm.expectEmit(true, true, true, true, address(subsidy));
        emit SubsidyClaimed(nullifier, sessionId, period, 60_000 gwei);

        subsidy.claimSubsidy(nullifier, sessionId, addrs, items);

        assertEq(subsidy.getBudget(nullifier), 60_000 gwei, "summed budget");
        assertTrue(subsidy.isAuthorized(ALICE, nullifier));
        assertTrue(subsidy.isAuthorized(BOB, nullifier));
        assertTrue(subsidy.isClaimed(nullifier, SCHEMA_POH));
        assertTrue(subsidy.isClaimed(nullifier, SCHEMA_PHONE));
    }

    function test_claimSubsidy_happyPath_emptyAddAddresses_allowed() public {
        _setBudgets();
        uint256 nullifier = 0xA003;
        address[] memory addrs = new address[](0);
        ISubsidyAccounting.ClaimItem[] memory items = _items1(SCHEMA_POH, 7);

        subsidy.claimSubsidy(nullifier, 1, addrs, items);

        assertEq(subsidy.getBudget(nullifier), 50_000 gwei);
        assertFalse(subsidy.isAuthorized(ALICE, nullifier));
        assertEq(subsidy.getNullifiers(ALICE).length, 0, "no reverse-index entries");
    }

    function test_claimSubsidy_happyPath_zeroBudgetItems() public {
        // No setCredentialBudget call — schema budgets default to zero.
        uint256 nullifier = 0xA004;
        ISubsidyAccounting.ClaimItem[] memory items = _items1(SCHEMA_POH, 7);

        uint64 period = uint64(block.timestamp / PERIOD_LENGTH);
        vm.expectEmit(true, true, true, true, address(subsidy));
        emit SubsidyClaimed(nullifier, 1, period, 0);

        subsidy.claimSubsidy(nullifier, 1, _addrs1(ALICE), items);
        assertEq(subsidy.getBudget(nullifier), 0, "zero-budget record opens");
        assertTrue(subsidy.isClaimed(nullifier, SCHEMA_POH), "still marked claimed");
    }

    function test_claimSubsidy_revertIf_emptyItems() public {
        ISubsidyAccounting.ClaimItem[] memory items = new ISubsidyAccounting.ClaimItem[](0);
        vm.expectRevert(ISubsidyAccounting.EmptyItems.selector);
        subsidy.claimSubsidy(0xA005, 1, _addrs1(ALICE), items);
    }

    function test_claimSubsidy_revertIf_recordAlreadyExists_samePeriod() public {
        _setBudgets();
        uint256 nullifier = 0xA006;
        subsidy.claimSubsidy(nullifier, 1, _addrs1(ALICE), _items1(SCHEMA_POH, 1));

        vm.expectRevert(ISubsidyAccounting.RecordAlreadyExists.selector);
        subsidy.claimSubsidy(nullifier, 2, _addrs1(BOB), _items1(SCHEMA_PHONE, 2));
    }

    function test_claimSubsidy_revertIf_duplicateIssuerSchemaId_inPayload() public {
        _setBudgets();
        ISubsidyAccounting.ClaimItem[] memory items = _items2(SCHEMA_POH, SCHEMA_POH, 1, 2);
        vm.expectRevert(abi.encodeWithSelector(ISubsidyAccounting.DuplicateIssuerSchemaId.selector, SCHEMA_POH));
        subsidy.claimSubsidy(0xA008, 1, _addrs1(ALICE), items);
    }

    function test_claimSubsidy_revertIf_duplicateAuthorizedAddress() public {
        _setBudgets();
        address[] memory addrs = _addrs2(ALICE, ALICE);
        vm.expectRevert(abi.encodeWithSelector(ISubsidyAccounting.DuplicateAuthorizedAddress.selector, ALICE));
        subsidy.claimSubsidy(0xA00A, 1, addrs, _items1(SCHEMA_POH, 1));
    }

    function test_claimSubsidy_revertIf_zeroAuthorizedAddress() public {
        _setBudgets();
        address[] memory addrs = _addrs2(ALICE, address(0));
        vm.expectRevert(ISubsidyAccounting.AddressZero.selector);
        subsidy.claimSubsidy(0xA00B, 1, addrs, _items1(SCHEMA_POH, 1));
    }

    function test_claimSubsidy_revertIf_verifierRejects_atomicRollback() public {
        _setBudgets();
        verifier.setShouldAccept(false);
        uint256 nullifier = 0xA00F;

        vm.expectRevert(MockWorldIDVerifier.MockVerifierRejected.selector);
        subsidy.claimSubsidy(nullifier, 1, _addrs1(ALICE), _items1(SCHEMA_POH, 1));

        // Atomic rollback: state untouched.
        assertEq(subsidy.getBudget(nullifier), 0, "no record opened");
        assertFalse(subsidy.isAuthorized(ALICE, nullifier));
        assertFalse(subsidy.isClaimed(nullifier, SCHEMA_POH));
        assertEq(subsidy.getNullifiers(ALICE).length, 0);
    }

    function test_claimSubsidy_revertIf_budgetOverflow() public {
        // Two items each at max uint128 budget overflow uint128 when summed.
        uint256 huge = uint256(type(uint128).max);
        vm.startPrank(OWNER);
        subsidy.setCredentialBudget(SCHEMA_POH, huge);
        subsidy.setCredentialBudget(SCHEMA_PHONE, huge);
        vm.stopPrank();

        ISubsidyAccounting.ClaimItem[] memory items = _items2(SCHEMA_POH, SCHEMA_PHONE, 1, 2);
        vm.expectRevert(abi.encodeWithSelector(ISubsidyAccounting.BudgetOverflow.selector, huge + huge));
        subsidy.claimSubsidy(0xA011, 1, _addrs1(ALICE), items);
    }

    function test_claimSubsidy_emptyAddAddresses_dormantRecord() public {
        _setBudgets();
        uint256 nullifier = 0xD0001;
        address[] memory empty = new address[](0);
        subsidy.claimSubsidy(nullifier, 1, empty, _items1(SCHEMA_POH, 1));
        assertEq(subsidy.getBudget(nullifier), 50_000 gwei, "budget credited even without authorisers");
        assertFalse(subsidy.isAuthorized(ALICE, nullifier), "no one is authorised");
        assertEq(subsidy.getNullifiers(ALICE).length, 0, "alice has no reverse-index entry");
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                            SET AUTHORIZED                               ///
    ///////////////////////////////////////////////////////////////////////////////

    bytes32 internal constant SET_AUTHORIZED_TAG = "SET_AUTHORIZED";
    bytes32 internal constant CLAIM_ADDITIONAL_CREDENTIAL_TAG = "CLAIM_ADDITIONAL_CREDENTIAL";

    event AuthorizedSetUpdated(uint256 indexed nullifier, address[] newSet);
    event AdditionalCredentialClaimed(uint256 indexed nullifier, uint64 indexed issuerSchemaId, uint256 budgetWei);

    function _setAuthorizedSignal(uint256 nullifier, uint64 nonce, address[] memory newSet, address sender)
        internal
        pure
        returns (uint256)
    {
        SubsidyAccountingImplV1.SetAuthorizedSignal memory s = SubsidyAccountingImplV1.SetAuthorizedSignal({
            tag: SET_AUTHORIZED_TAG, nullifier: nullifier, nonce: nonce, newSet: newSet, msgSender: sender
        });
        return uint256(keccak256(abi.encode(s))) >> 8;
    }

    function _claimAdditionalCredentialSignal(uint256 nullifier, address sender) internal pure returns (uint256) {
        SubsidyAccountingImplV1.ClaimAdditionalCredentialSignal memory s = SubsidyAccountingImplV1
            .ClaimAdditionalCredentialSignal({
            tag: CLAIM_ADDITIONAL_CREDENTIAL_TAG, nullifier: nullifier, msgSender: sender
        });
        return uint256(keccak256(abi.encode(s))) >> 8;
    }

    function _seedClaim(uint256 nullifier, uint256 sessionId, address[] memory addrs) internal {
        _setBudgets();
        subsidy.claimSubsidy(nullifier, sessionId, addrs, _items1(SCHEMA_POH, 1));
    }

    function test_setAuthorized_happyPath_replacesSetAndBumpsNonce() public {
        uint256 nullifier = 0xB001;
        uint256 sessionId = 0x5E5510;
        _seedClaim(nullifier, sessionId, _addrs1(ALICE));

        address[] memory newSet = _addrs1(BOB);
        uint64 period = uint64(block.timestamp / PERIOD_LENGTH);
        uint256 sigHash = _setAuthorizedSignal(nullifier, 0, newSet, address(this));
        uint256[2] memory sn = [uint256(0xAA), uint256(0xBB)];

        vm.expectCall(
            address(verifier),
            abi.encodeCall(
                IWorldIDVerifier.verifySession,
                (
                    WORLD_CHAIN_RP_ID,
                    PROOF_NONCE,
                    sigHash,
                    period * PERIOD_LENGTH,
                    uint64(0),
                    CREDENTIAL_GENESIS_ISSUED_AT_MIN,
                    sessionId,
                    sn,
                    _proof(7)
                )
            )
        );
        vm.expectEmit(true, true, true, true, address(subsidy));
        emit AuthorizedSetUpdated(nullifier, newSet);

        subsidy.setAuthorized(nullifier, 0, newSet, sn[0], sn[1], _proof(7));

        assertFalse(subsidy.isAuthorized(ALICE, nullifier), "alice removed");
        assertTrue(subsidy.isAuthorized(BOB, nullifier), "bob added");
        assertEq(subsidy.getNullifiers(ALICE).length, 0, "alice reverse-index cleared");
        uint256[] memory bobNs = subsidy.getNullifiers(BOB);
        assertEq(bobNs.length, 1);
        assertEq(bobNs[0], nullifier);
        // Nonce bump: a second call with the same nonce 0 must now revert StaleUpdateNonce.
        vm.expectRevert(abi.encodeWithSelector(ISubsidyAccounting.StaleUpdateNonce.selector, uint64(0), uint64(1)));
        subsidy.setAuthorized(nullifier, 0, newSet, sn[0], sn[1], _proof(7));
    }

    function test_setAuthorized_emptyNewSet_revokesAll() public {
        uint256 nullifier = 0xB002;
        _seedClaim(nullifier, 1, _addrs2(ALICE, BOB));
        address[] memory empty = new address[](0);

        subsidy.setAuthorized(nullifier, 0, empty, 1, 2, _proof(3));

        assertFalse(subsidy.isAuthorized(ALICE, nullifier));
        assertFalse(subsidy.isAuthorized(BOB, nullifier));
        assertEq(subsidy.getNullifiers(ALICE).length, 0);
        assertEq(subsidy.getNullifiers(BOB).length, 0);
    }

    function test_setAuthorized_revertIf_duplicateInNewSet() public {
        uint256 nullifier = 0xB003;
        _seedClaim(nullifier, 1, _addrs1(ALICE));
        address[] memory dup = _addrs2(BOB, BOB);

        vm.expectRevert(abi.encodeWithSelector(ISubsidyAccounting.DuplicateAuthorizedAddress.selector, BOB));
        subsidy.setAuthorized(nullifier, 0, dup, 1, 2, _proof(3));
    }

    function test_setAuthorized_revertIf_staleNonce() public {
        uint256 nullifier = 0xB004;
        _seedClaim(nullifier, 1, _addrs1(ALICE));
        address[] memory s = _addrs1(BOB);

        vm.expectRevert(abi.encodeWithSelector(ISubsidyAccounting.StaleUpdateNonce.selector, uint64(7), uint64(0)));
        subsidy.setAuthorized(nullifier, 7, s, 1, 2, _proof(3));
    }

    function test_setAuthorized_revertIf_verifierRejects() public {
        uint256 nullifier = 0xB005;
        _seedClaim(nullifier, 1, _addrs1(ALICE));
        verifier.setShouldAccept(false);
        address[] memory s = _addrs1(BOB);

        vm.expectRevert(MockWorldIDVerifier.MockVerifierRejected.selector);
        subsidy.setAuthorized(nullifier, 0, s, 1, 2, _proof(3));
        // State must be untouched after revert.
        assertTrue(subsidy.isAuthorized(ALICE, nullifier), "alice still authorised");
        assertFalse(subsidy.isAuthorized(BOB, nullifier));
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                       CLAIM ADDITIONAL CREDENTIAL                       ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_claimAdditionalCredential_happyPath_addsBudgetAndMarksClaimed() public {
        uint256 nullifier = 0xC001;
        uint256 sessionId = 0x5E5511;
        _seedClaim(nullifier, sessionId, _addrs1(ALICE));

        uint64 period = uint64(block.timestamp / PERIOD_LENGTH);
        uint256 sigHash = _claimAdditionalCredentialSignal(nullifier, address(this));
        uint256[2] memory sn = [uint256(0xCC), uint256(0xDD)];

        vm.expectCall(
            address(verifier),
            abi.encodeCall(
                IWorldIDVerifier.verifySession,
                (
                    WORLD_CHAIN_RP_ID,
                    PROOF_NONCE,
                    sigHash,
                    period * PERIOD_LENGTH,
                    SCHEMA_NFC,
                    CREDENTIAL_GENESIS_ISSUED_AT_MIN,
                    sessionId,
                    sn,
                    _proof(9)
                )
            )
        );
        vm.expectEmit(true, true, true, true, address(subsidy));
        emit AdditionalCredentialClaimed(nullifier, SCHEMA_NFC, 20_000 gwei);

        subsidy.claimAdditionalCredential(nullifier, SCHEMA_NFC, sn[0], sn[1], _proof(9));

        assertEq(subsidy.getBudget(nullifier), 50_000 gwei + 20_000 gwei, "budget summed");
        assertTrue(subsidy.isClaimed(nullifier, SCHEMA_NFC));
    }

    function test_claimAdditionalCredential_revertIf_alreadyClaimed() public {
        uint256 nullifier = 0xC002;
        _seedClaim(nullifier, 1, _addrs1(ALICE));

        vm.expectRevert(abi.encodeWithSelector(ISubsidyAccounting.CredentialAlreadyClaimed.selector, SCHEMA_POH));
        subsidy.claimAdditionalCredential(nullifier, SCHEMA_POH, 1, 2, _proof(3));
    }

    function test_claimAdditionalCredential_revertIf_verifierRejects() public {
        uint256 nullifier = 0xC003;
        _seedClaim(nullifier, 1, _addrs1(ALICE));
        verifier.setShouldAccept(false);

        vm.expectRevert(MockWorldIDVerifier.MockVerifierRejected.selector);
        subsidy.claimAdditionalCredential(nullifier, SCHEMA_NFC, 1, 2, _proof(3));
        assertFalse(subsidy.isClaimed(nullifier, SCHEMA_NFC));
        assertEq(subsidy.getBudget(nullifier), 50_000 gwei, "budget unchanged");
    }

    function test_claimAdditionalCredential_revertIf_budgetOverflow() public {
        uint256 nullifier = 0xC004;
        uint256 huge = uint256(type(uint128).max);
        vm.startPrank(OWNER);
        subsidy.setCredentialBudget(SCHEMA_POH, huge);
        subsidy.setCredentialBudget(SCHEMA_NFC, huge);
        vm.stopPrank();
        subsidy.claimSubsidy(nullifier, 1, _addrs1(ALICE), _items1(SCHEMA_POH, 1));

        vm.expectRevert(abi.encodeWithSelector(ISubsidyAccounting.BudgetOverflow.selector, huge + huge));
        subsidy.claimAdditionalCredential(nullifier, SCHEMA_NFC, 1, 2, _proof(3));
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                  REGISTRATION VERSION + PERIOD RESET                    ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_setWorldIDVerifier_invalidatesExistingRecords_structurally() public {
        uint256 nullifier = 0xE001;
        _seedClaim(nullifier, 1, _addrs1(ALICE));
        assertEq(subsidy.getBudget(nullifier), 50_000 gwei, "pre-swap budget present");
        assertTrue(subsidy.isAuthorized(ALICE, nullifier));

        IWorldIDVerifier newVerifier = IWorldIDVerifier(address(new MockWorldIDVerifier(true)));
        vm.prank(OWNER);
        subsidy.setWorldIDVerifier(newVerifier);

        // The pre-swap record lives under the prior `_registrationVersion`; currentAction() now
        // resolves to a different outer key, so all current-action views see an empty slot.
        assertEq(subsidy.getBudget(nullifier), 0, "post-swap budget structurally absent");
        assertEq(subsidy.getBudget(ALICE), 0, "address-side budget also gone");
        assertFalse(subsidy.isAuthorized(ALICE, nullifier));
        assertEq(subsidy.getNullifiers(ALICE).length, 0);
        assertFalse(subsidy.isClaimed(nullifier, SCHEMA_POH));
    }

    function test_period_structuralReset_acrossBoundary() public {
        uint256 nullifier = 0xF001;
        _seedClaim(nullifier, 1, _addrs1(ALICE));
        assertEq(subsidy.getBudget(nullifier), 50_000 gwei, "claim period budget present");

        vm.warp(block.timestamp + PERIOD_LENGTH);

        assertEq(subsidy.getBudget(nullifier), 0, "next period sees no record");
        assertFalse(subsidy.isAuthorized(ALICE, nullifier));
        assertEq(subsidy.getNullifiers(ALICE).length, 0);

        // Same nullifier may be re-claimed under the new action key without RecordAlreadyExists.
        subsidy.claimSubsidy(nullifier, 2, _addrs1(BOB), _items1(SCHEMA_PHONE, 9));
        assertEq(subsidy.getBudget(nullifier), 10_000 gwei);
    }
}
