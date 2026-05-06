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

    function test_claimSubsidy_revertsNotImplemented() public {
        ISubsidyAccounting.ClaimItem[] memory items = new ISubsidyAccounting.ClaimItem[](0);
        address[] memory addrs = new address[](0);
        vm.expectRevert(ISubsidyAccounting.NotImplemented.selector);
        subsidy.claimSubsidy(0, 0, addrs, items);
    }

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
}
