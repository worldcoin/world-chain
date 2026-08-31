// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";

import {IERC20StakingVault} from "../../src/dispute/interfaces/IERC20StakingVault.sol";
import {MockBondToken} from "../../test/mocks/MockBondToken.sol";

/// @notice Tops persistent devnet proof-system accounts up to configured reusable vault balances.
/// @dev Devnet only: this script relies on the unrestricted mint function exposed by `MockBondToken`.
///      Run it before bond-paying services start so balances remain stable during broadcasting.
contract FundProofBondAccounts is Script {
    uint256 internal constant DEFAULT_VAULT_BALANCE_TARGET = 10_000e18;

    function run() external {
        uint256 privateKey = vm.envUint("PRIVATE_KEY");
        address funder = vm.addr(privateKey);
        IERC20StakingVault vault = IERC20StakingVault(vm.envAddress("ERC20_STAKING_VAULT"));
        MockBondToken bondToken = MockBondToken(address(vault.token()));
        address[] memory accounts = vm.envAddress("BOND_ACCOUNTS", ",");
        uint256 target = vm.envOr("VAULT_BALANCE_TARGET", DEFAULT_VAULT_BALANCE_TARGET);

        _validate(accounts, target);

        vm.startBroadcast(privateKey);
        _fundAccounts(bondToken, vault, funder, accounts, target);
        vm.stopBroadcast();
    }

    function _fundAccounts(
        MockBondToken bondToken,
        IERC20StakingVault vault,
        address funder,
        address[] memory accounts,
        uint256 target
    ) internal {
        for (uint256 i = 0; i < accounts.length; i++) {
            bool duplicate;
            for (uint256 j = 0; j < i; j++) {
                if (accounts[j] == accounts[i]) {
                    duplicate = true;
                    break;
                }
            }
            if (duplicate) continue;
            _topUp(bondToken, vault, funder, accounts[i], target);
        }
    }

    function _validate(address[] memory accounts, uint256 target) internal pure {
        require(accounts.length != 0, "FundProofBondAccounts: accounts missing");
        require(target != 0, "FundProofBondAccounts: target missing");
        for (uint256 i = 0; i < accounts.length; i++) {
            require(accounts[i] != address(0), "FundProofBondAccounts: account missing");
        }
    }

    function _topUp(MockBondToken bondToken, IERC20StakingVault vault, address funder, address account, uint256 target)
        internal
    {
        uint256 available = vault.availableBalance(account);
        if (available >= target) return;

        uint256 amount = target - available;
        bondToken.mint(funder, amount);
        bondToken.approve(address(vault), amount);
        vault.deposit(account, amount);

        require(vault.availableBalance(account) >= target, "FundProofBondAccounts: top-up failed");
        require(bondToken.allowance(funder, address(vault)) == 0, "FundProofBondAccounts: allowance remains");
    }
}
