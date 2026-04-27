// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Script} from "forge-std/Script.sol";
import {console} from "forge-std/console.sol";
import {FeeEscrow} from "../../src/fees/FeeEscrow.sol";
import {FeeRecipient} from "../../src/fees/FeeRecipient.sol";
import {MockChainLinkPriceFeed} from "../../test/mocks/MockChainLinkPriceFeed.sol";

/// @title DeployFeeVaults
/// @notice Deploys FeeEscrow and FeeRecipient contracts for devnet
/// @dev Deploys mock oracles since ChainLink feeds are not available on devnet
contract DeployFeeVaults is Script {
    /// @notice Mock ETH price in USD (8 decimals) - $2500
    int192 public constant MOCK_ETH_PRICE = 2500e8;

    /// @notice Mock WLD price in USD (8 decimals) - $1.50
    int192 public constant MOCK_WLD_PRICE = 1.5e8;

    /// @notice Oracle decimals (standard ChainLink decimals)
    uint8 public constant ORACLE_DECIMALS = 8;

    /// @notice Price validity duration (1 day)
    uint32 public constant PRICE_VALIDITY = 1 days;

    /// @notice Minimum interval between burns (1 hour for devnet)
    uint256 public constant MINIMUM_INTERVAL = 1 hours;

    /// @notice Distribution ratio (50% to burn, 50% kept)
    uint24 public constant DISTRIBUTION_RATIO = 50000;

    address public wldUsdOracle;
    address public ethUsdOracle;
    address public feeEscrow;
    address public feeRecipient;

    function run() public {
        uint256 privateKey = vm.envUint("PRIVATE_KEY");
        address deployer = vm.addr(privateKey);

        // Optional: Allow override of fee vault recipient
        address recipient = vm.envOr("FEE_VAULT_RECIPIENT", deployer);

        console.log("Deploying Fee Vaults for devnet...");
        console.log("Deployer:", deployer);
        console.log("Fee Vault Recipient:", recipient);

        vm.startBroadcast(privateKey);

        deployMockOracles();
        deployFeeEscrow(deployer);
        deployFeeRecipient(recipient, deployer);

        vm.stopBroadcast();

        logDeployment();
    }

    function deployMockOracles() internal {
        // Deploy mock WLD/USD oracle
        wldUsdOracle = address(new MockChainLinkPriceFeed(MOCK_WLD_PRICE, ORACLE_DECIMALS, PRICE_VALIDITY));
        console.log("MockChainLinkPriceFeed (WLD/USD) deployed at:", wldUsdOracle);

        // Deploy mock ETH/USD oracle
        ethUsdOracle = address(new MockChainLinkPriceFeed(MOCK_ETH_PRICE, ORACLE_DECIMALS, PRICE_VALIDITY));
        console.log("MockChainLinkPriceFeed (ETH/USD) deployed at:", ethUsdOracle);
    }

    function deployFeeEscrow(address owner) internal {
        feeEscrow = address(new FeeEscrow(owner, MINIMUM_INTERVAL));
        console.log("FeeEscrow deployed at:", feeEscrow);

        // Configure mock oracles
        FeeEscrow(payable(feeEscrow)).setWLDUSDOracle(wldUsdOracle);
        FeeEscrow(payable(feeEscrow)).setETHUSDOracle(ethUsdOracle);
        console.log("FeeEscrow oracles configured");
    }

    function deployFeeRecipient(address recipient, address owner) internal {
        feeRecipient = address(new FeeRecipient(feeEscrow, recipient, DISTRIBUTION_RATIO, owner));
        console.log("FeeRecipient deployed at:", feeRecipient);
    }

    function logDeployment() internal view {
        console.log("");
        console.log("=== Fee Vault Deployment Summary ===");
        console.log("WLD/USD Oracle:", wldUsdOracle);
        console.log("ETH/USD Oracle:", ethUsdOracle);
        console.log("FeeEscrow:", feeEscrow);
        console.log("FeeRecipient:", feeRecipient);
        console.log("Minimum Interval:", MINIMUM_INTERVAL);
        console.log("Distribution Ratio:", DISTRIBUTION_RATIO);
    }
}
