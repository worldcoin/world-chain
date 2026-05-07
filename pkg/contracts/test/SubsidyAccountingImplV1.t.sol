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
    address internal constant CONSUMER = address(0xC0);

    event SubsidyAccountingImplInitialized(IWorldIDVerifier indexed worldIDVerifier, address indexed owner);
    event WorldIDVerifierSet(address indexed worldIDVerifier);
    event BudgetConsumerSet(address indexed budgetConsumer);
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
        assertEq(fresh.budgetConsumer(), address(0), "consumer defaults to zero");
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
    ///                          ADMIN: setBudgetConsumer                       ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_setBudgetConsumer_byOwner_persists_andEmits() public {
        vm.expectEmit(true, true, true, true, address(subsidy));
        emit BudgetConsumerSet(CONSUMER);

        vm.prank(OWNER);
        subsidy.setBudgetConsumer(CONSUMER);

        assertEq(subsidy.budgetConsumer(), CONSUMER);
    }

    function test_setBudgetConsumer_zeroAddressAllowed() public {
        vm.prank(OWNER);
        subsidy.setBudgetConsumer(CONSUMER);
        assertEq(subsidy.budgetConsumer(), CONSUMER);

        vm.expectEmit(true, true, true, true, address(subsidy));
        emit BudgetConsumerSet(address(0));

        vm.prank(OWNER);
        subsidy.setBudgetConsumer(address(0));
        assertEq(subsidy.budgetConsumer(), address(0), "deprovisioning is permitted");
    }

    function test_setBudgetConsumer_revertIf_notOwner() public {
        vm.prank(ATTACKER);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, ATTACKER));
        subsidy.setBudgetConsumer(CONSUMER);
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

    function test_setCredentialBudget_revertIf_overflowsUint64() public {
        uint256 oversized = uint256(type(uint64).max) + 1;
        vm.prank(OWNER);
        vm.expectRevert(abi.encodeWithSelector(ISubsidyAccounting.IssuerSchemaIdOverflow.selector, oversized));
        subsidy.setCredentialBudget(oversized, 1 ether);
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

    function test_isClaimed_returnsFalse_forOversizedSchemaId() public view {
        // Permissive view path: oversized issuerSchemaId returns false rather than reverts.
        uint256 oversized = uint256(type(uint64).max) + 1;
        assertFalse(subsidy.isClaimed(0xDEADBEEF, oversized));
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                       ENTRY-POINT STUBS (NotImplemented)                ///
    ///////////////////////////////////////////////////////////////////////////////

    function test_claimAdditionalCredential_revertsNotImplemented() public {
        uint256[2] memory sessionNullifier;
        uint256[5] memory proof;
        vm.expectRevert(ISubsidyAccounting.NotImplemented.selector);
        subsidy.claimAdditionalCredential(0, 0, sessionNullifier, proof);
    }

    function test_updateAddresses_revertsNotImplemented() public {
        address[] memory empty = new address[](0);
        uint256[2] memory sessionNullifier;
        uint256[5] memory proof;
        vm.expectRevert(ISubsidyAccounting.NotImplemented.selector);
        subsidy.updateAddresses(0, 0, empty, empty, sessionNullifier, proof);
    }

    function test_consumeBudget_revertsNotImplemented_whenCalledByConsumer() public {
        // Authorize a consumer first so we hit the NotImplemented stub, not the ACL.
        vm.prank(OWNER);
        subsidy.setBudgetConsumer(CONSUMER);

        vm.prank(CONSUMER);
        vm.expectRevert(ISubsidyAccounting.NotImplemented.selector);
        subsidy.consumeBudget(address(0x1234), 21000, 1 gwei);
    }

    function test_consumeBudget_revertsNotBudgetConsumer_whenCalledByOther() public {
        vm.prank(OWNER);
        subsidy.setBudgetConsumer(CONSUMER);

        vm.prank(ATTACKER);
        vm.expectRevert(ISubsidyAccounting.NotBudgetConsumer.selector);
        subsidy.consumeBudget(address(0x1234), 21000, 1 gwei);
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
        return uint256(keccak256(abi.encodePacked("period_proof", period))) >> 8;
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

    function test_claimSubsidy_revertIf_issuerSchemaIdOverflow() public {
        uint256 oversized = uint256(type(uint64).max) + 1;
        ISubsidyAccounting.ClaimItem[] memory items = new ISubsidyAccounting.ClaimItem[](1);
        items[0] = ISubsidyAccounting.ClaimItem({issuerSchemaId: oversized, proof: _proof(1)});
        vm.expectRevert(abi.encodeWithSelector(ISubsidyAccounting.IssuerSchemaIdOverflow.selector, oversized));
        subsidy.claimSubsidy(0xA009, 1, _addrs1(ALICE), items);
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

    function test_claimSubsidy_revertIf_tooManyNullifiers() public {
        _setBudgets();
        // Authorise ALICE under MAX_NULLIFIERS_PER_ADDRESS = 16 records, then attempt the 17th.
        for (uint256 i = 0; i < 16; ++i) {
            uint256 nullifier = 0xC0DE0000 + i;
            // Each call needs a distinct schemaId to avoid DuplicateIssuerSchemaId carrying
            // across periods within the same nullifier — but here nullifiers differ, so reuse
            // is fine. Bind to SCHEMA_POH each time.
            subsidy.claimSubsidy(nullifier, i + 1, _addrs1(ALICE), _items1(SCHEMA_POH, i));
        }
        vm.expectRevert(abi.encodeWithSelector(ISubsidyAccounting.TooManyNullifiers.selector, ALICE));
        subsidy.claimSubsidy(0xC0DE0010, 99, _addrs1(ALICE), _items1(SCHEMA_POH, 99));
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
}
