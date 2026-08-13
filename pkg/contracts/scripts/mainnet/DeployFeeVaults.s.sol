// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.26;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";
import {FeeEscrow} from "../../src/fees/FeeEscrow.sol";
import {FeeRecipient} from "../../src/fees/FeeRecipient.sol";

contract DeployScript is Script {
    bool deployFeeEscrow = true;
    bool deployFeeRecipient = true;

    uint256 internal PRIVATE_KEY;

    /// @notice Primary Fee Vault recipient
    address public RECIPIENT;

    /// @notice Minimum time between burn executions
    uint256 public MINIMUM_INTERVAL = 24 hours;

    /// @notice Owner of the deployed FeeEscrow contract
    address public OWNER = msg.sender;

    /// @notice Ratio of incoming ETH sent to burn (in basis points, SCALE = 100000)
    uint24 public DISTRIBUTION_RATIO = 50000;

    /// @notice Address that receives withdrawn funds
    address public FEE_ESCROW;

    /// @notice Fee Vault contract address
    address public FEE_RECIPIENT;

    function init() internal {
        PRIVATE_KEY = vm.envUint("PRIVATE_KEY");
        MINIMUM_INTERVAL = vm.envOr("MINIMUM_INTERVAL", MINIMUM_INTERVAL);
        OWNER = vm.envOr("OWNER", OWNER);
        DISTRIBUTION_RATIO = uint24(vm.envOr("DISTRIBUTION_RATIO", DISTRIBUTION_RATIO));
        RECIPIENT = vm.envOr("RECIPIENT", RECIPIENT);
    }

    function setUp() public {
        init();
    }

    function run() public {
        vm.startBroadcast(PRIVATE_KEY);

        console2.log("Deploying FeeEscrow...");
        FEE_ESCROW = address(new FeeEscrow(OWNER, MINIMUM_INTERVAL));
        console2.log("FeeEscrow deployed at:", FEE_ESCROW);

        console2.log("Deploying FeeRecipient...");

        FEE_RECIPIENT = address(new FeeRecipient(FEE_ESCROW, RECIPIENT, DISTRIBUTION_RATIO, OWNER));
        console2.log("FeeRecipient deployed at:", FEE_RECIPIENT);

        writeConfig();

        vm.stopBroadcast();
    }

    // Write Config to JSON
    function writeConfig() public {
        string memory path = "script/summary.json";
        string memory obj = "deployment";

        vm.serializeAddress(obj, "recipient", RECIPIENT);
        vm.serializeUint(obj, "minimumInterval", MINIMUM_INTERVAL);
        vm.serializeAddress(obj, "owner", OWNER);
        vm.serializeUint(obj, "distributionRatio", DISTRIBUTION_RATIO);
        vm.serializeAddress(obj, "feeEscrow", FEE_ESCROW);
        string memory json = vm.serializeAddress(obj, "feeRecipient", FEE_RECIPIENT);

        vm.writeJson(json, path);
        console2.log("Configuration written to", path);
    }
}
