// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {Test, console2} from "forge-std/Test.sol";
import {ChainlinkWldEthOracle} from "../src/oracle/ChainlinkWldEthOracle.sol";
import {IAggregatorV3} from "../src/interfaces/IAggregatorV3.sol";

/**
 * @notice Sanity-checks the real World Chain feeds. Skipped unless
 *         `WORLDCHAIN_RPC_URL` is set:
 *
 *         WORLDCHAIN_RPC_URL=https://worldchain-mainnet.g.alchemy.com/public \
 *           forge test --match-contract ChainlinkWldEthOracleForkTest
 */
contract ChainlinkWldEthOracleForkTest is Test {
    address constant WLD_USD_FEED = 0x8Bb2943AB030E3eE05a58d9832525B4f60A97FA0;
    address constant ETH_USD_FEED = 0xe1d72a719171DceAB9499757EB9d5AEb9e8D64A6;

    bool skipped;

    function setUp() public {
        string memory rpc = vm.envOr("WORLDCHAIN_RPC_URL", string(""));
        if (bytes(rpc).length == 0) {
            skipped = true;
            return;
        }
        vm.createSelectFork(rpc);
    }

    function test_LiveFeedsPriceGas() public {
        vm.skip(skipped);

        assertEq(IAggregatorV3(WLD_USD_FEED).description(), "WLD/USD");
        assertEq(IAggregatorV3(ETH_USD_FEED).description(), "ETH/USD");

        ChainlinkWldEthOracle oracle =
            new ChainlinkWldEthOracle(IAggregatorV3(WLD_USD_FEED), IAggregatorV3(ETH_USD_FEED), 1 hours);

        (uint256 wldUsd, uint256 ethUsd) = oracle.prices();
        console2.log("WLD/USD (1e18):", wldUsd);
        console2.log("ETH/USD (1e18):", ethUsd);

        // A 0.05 ETH gas budget should cost a sane, non-trivial amount of WLD.
        uint256 wld = oracle.wldForEth(0.05 ether);
        console2.log("WLD for 0.05 ETH:", wld);
        assertGt(wld, 0);
        assertApproxEqRel(oracle.ethForWld(wld), 0.05 ether, 1e12);
    }
}
