// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {FundProofBondAccounts} from "../../scripts/devnet/FundProofBondAccounts.s.sol";
import {IWLDStakingVault} from "../../src/dispute/interfaces/IWLDStakingVault.sol";
import {OPStackFixtures} from "./OPStackFixtures.sol";
import {MockWLD} from "../mocks/MockWLD.sol";

contract FundProofBondAccountsHarness is FundProofBondAccounts {
    function validate(address[] memory accounts, uint256 target) external pure {
        _validate(accounts, target);
    }

    function fund(MockWLD wld, IWLDStakingVault vault, address[] memory accounts, uint256 target) external {
        _validate(accounts, target);
        _fundAccounts(wld, vault, address(this), accounts, target);
    }
}

contract FundProofBondAccountsTest is OPStackFixtures {
    uint256 internal constant FUNDING_KEY = 0xF00D;
    uint256 internal constant TARGET = 10_000e18;

    FundProofBondAccountsHarness internal funding;
    address internal first = makeAddr("first-bond-account");
    address internal second = makeAddr("second-bond-account");

    function setUp() public override {
        super.setUp();
        funding = new FundProofBondAccountsHarness();
    }

    function test_run_topsEachAccountUpToTarget() public {
        uint256 existing = 2_000e18;
        wld.mint(address(this), existing);
        wld.approve(address(bondVault), existing);
        bondVault.deposit(first, existing);
        _setAccounts(first, second);
        vm.setEnv("PRIVATE_KEY", vm.toString(FUNDING_KEY));
        vm.setEnv("WLD_STAKING_VAULT", vm.toString(address(bondVault)));
        vm.setEnv("VAULT_BALANCE_TARGET", vm.toString(TARGET));

        uint256 supplyBefore = wld.totalSupply();
        funding.run();

        assertEq(bondVault.availableBalance(first), TARGET);
        assertEq(bondVault.availableBalance(second), TARGET);
        assertEq(wld.totalSupply() - supplyBefore, TARGET * 2 - existing);
        assertEq(wld.balanceOf(vm.addr(FUNDING_KEY)), 0);
        assertEq(wld.allowance(vm.addr(FUNDING_KEY), address(bondVault)), 0);
    }

    function test_fund_isIdempotent() public {
        address[] memory accounts = _accounts(first, second);
        funding.fund(wld, bondVault, accounts, TARGET);
        uint256 supplyAfterFirstRun = wld.totalSupply();
        uint256 vaultBalanceAfterFirstRun = wld.balanceOf(address(bondVault));

        funding.fund(wld, bondVault, accounts, TARGET);

        assertEq(wld.totalSupply(), supplyAfterFirstRun);
        assertEq(wld.balanceOf(address(bondVault)), vaultBalanceAfterFirstRun);
        assertEq(bondVault.availableBalance(first), TARGET);
        assertEq(bondVault.availableBalance(second), TARGET);
    }

    function test_fund_targetsAvailableBalanceWithoutCountingLockedBonds() public {
        _proposeAtAnchor();
        address[] memory accounts = new address[](1);
        accounts[0] = proposer;

        funding.fund(wld, bondVault, accounts, TARGET);

        assertEq(bondVault.availableBalance(proposer), TARGET);
        assertEq(wld.balanceOf(address(bondVault)), TARGET + PROPOSER_BOND + 100 * WLD_UNIT);
    }

    function test_fund_ignoresDuplicateAccountsAfterFirstTopUp() public {
        address[] memory accounts = _accounts(first, first);
        uint256 supplyBefore = wld.totalSupply();

        funding.fund(wld, bondVault, accounts, TARGET);

        assertEq(bondVault.availableBalance(first), TARGET);
        assertEq(wld.totalSupply() - supplyBefore, TARGET);
    }

    function test_validate_rejectsZeroAccount() public {
        address[] memory accounts = new address[](2);
        accounts[0] = first;

        vm.expectRevert("FundProofBondAccounts: account missing");
        funding.validate(accounts, TARGET);
    }

    function test_validate_rejectsEmptyAccounts() public {
        address[] memory accounts = new address[](0);

        vm.expectRevert("FundProofBondAccounts: accounts missing");
        funding.validate(accounts, TARGET);
    }

    function test_validate_rejectsZeroTarget() public {
        address[] memory accounts = _accounts(first, second);

        vm.expectRevert("FundProofBondAccounts: target missing");
        funding.validate(accounts, 0);
    }

    function _setAccounts(address account0, address account1) internal {
        vm.setEnv("BOND_ACCOUNTS", string.concat(vm.toString(account0), ",", vm.toString(account1)));
    }

    function _accounts(address account0, address account1) internal pure returns (address[] memory accounts) {
        accounts = new address[](2);
        accounts[0] = account0;
        accounts[1] = account1;
    }
}
