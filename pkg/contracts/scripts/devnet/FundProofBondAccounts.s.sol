// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";

import {IWLDStakingVault} from "../../src/dispute/interfaces/IWLDStakingVault.sol";
import {MockWLD} from "../../test/mocks/MockWLD.sol";

/// @notice Tops persistent devnet proof-system accounts up to configured reusable vault balances.
/// @dev Devnet only: this script relies on the unrestricted mint function exposed by `MockWLD`.
contract FundProofBondAccounts is Script {
    function run() external {
        uint256 privateKey = vm.envUint("PRIVATE_KEY");
        address funder = vm.addr(privateKey);
        MockWLD wld = MockWLD(vm.envAddress("WLD_TOKEN"));
        IWLDStakingVault vault = IWLDStakingVault(vm.envAddress("WLD_STAKING_VAULT"));
        address proposer = vm.envAddress("PROPOSER_ADDRESS");
        address challenger = vm.envAddress("CHALLENGER_ADDRESS");
        uint256 proposerTarget = vm.envUint("PROPOSER_VAULT_TARGET");
        uint256 challengerTarget = vm.envUint("CHALLENGER_VAULT_TARGET");

        require(vault.wld() == wld, "FundProofBondAccounts: vault WLD mismatch");
        require(proposer != address(0), "FundProofBondAccounts: proposer missing");
        require(challenger != address(0), "FundProofBondAccounts: challenger missing");

        vm.startBroadcast(privateKey);
        if (proposer == challenger) {
            _topUp(wld, vault, funder, proposer, proposerTarget + challengerTarget);
        } else {
            _topUp(wld, vault, funder, proposer, proposerTarget);
            _topUp(wld, vault, funder, challenger, challengerTarget);
        }
        vm.stopBroadcast();
    }

    function _topUp(MockWLD wld, IWLDStakingVault vault, address funder, address account, uint256 target) internal {
        uint256 available = vault.availableBalance(account);
        if (available >= target) return;

        uint256 amount = target - available;
        wld.mint(funder, amount);
        wld.approve(address(vault), amount);
        vault.depositFor(account, amount);

        require(vault.availableBalance(account) >= target, "FundProofBondAccounts: top-up failed");
        require(wld.allowance(funder, address(vault)) == 0, "FundProofBondAccounts: allowance remains");
    }
}
