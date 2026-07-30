// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {Test} from "forge-std/Test.sol";
import {ChainlinkWldEthOracle} from "../src/oracle/ChainlinkWldEthOracle.sol";
import {IAggregatorV3} from "../src/interfaces/IAggregatorV3.sol";
import {MockAggregatorV3} from "./mocks/Mocks.sol";

contract ChainlinkWldEthOracleTest is Test {
    // Live World Chain values at time of writing: WLD ≈ $0.3086, ETH ≈ $1921.14.
    int256 constant WLD_USD = 0.3086e18;
    int256 constant ETH_USD = 1921.14e18;
    uint256 constant MAX_STALENESS = 1 hours;

    MockAggregatorV3 wldFeed;
    MockAggregatorV3 ethFeed;
    ChainlinkWldEthOracle oracle;

    function setUp() public {
        vm.warp(1_785_445_819); // non-zero, realistic timestamp
        wldFeed = new MockAggregatorV3("WLD/USD", 18, WLD_USD);
        ethFeed = new MockAggregatorV3("ETH/USD", 18, ETH_USD);
        oracle = new ChainlinkWldEthOracle(wldFeed, ethFeed, MAX_STALENESS);
    }

    function test_WldForEth_UsesUsdCross() public view {
        // 1 ETH should cost ETH_USD / WLD_USD ≈ 6225 WLD.
        assertEq(oracle.wldForEth(1e18), (1e18 * uint256(ETH_USD)) / uint256(WLD_USD));
        assertApproxEqRel(oracle.wldForEth(1e18), 6224.6e18, 1e15); // within 0.1%
    }

    function test_EthForWld_IsInverse() public view {
        uint256 wld = oracle.wldForEth(1e18);
        assertApproxEqRel(oracle.ethForWld(wld), 1e18, 1e15);
    }

    function test_ParityWhenPricesEqual() public {
        wldFeed.setAnswer(ETH_USD);
        assertEq(oracle.wldForEth(1e18), 1e18);
        assertEq(oracle.ethForWld(1e18), 1e18);
    }

    /// @dev Feeds with fewer decimals must be normalised, not assumed to be 1e18.
    function test_NormalisesMixedFeedDecimals() public {
        MockAggregatorV3 wld8 = new MockAggregatorV3("WLD/USD", 8, 0.3086e8);
        MockAggregatorV3 eth8 = new MockAggregatorV3("ETH/USD", 8, 1921.14e8);
        ChainlinkWldEthOracle mixed = new ChainlinkWldEthOracle(wld8, ethFeed, MAX_STALENESS);
        ChainlinkWldEthOracle both8 = new ChainlinkWldEthOracle(wld8, eth8, MAX_STALENESS);

        assertApproxEqRel(mixed.wldForEth(1e18), oracle.wldForEth(1e18), 1e12);
        assertApproxEqRel(both8.wldForEth(1e18), oracle.wldForEth(1e18), 1e12);
    }

    function test_PricesAreScaledTo1e18() public view {
        (uint256 wldUsd, uint256 ethUsd) = oracle.prices();
        assertEq(wldUsd, uint256(WLD_USD));
        assertEq(ethUsd, uint256(ETH_USD));
    }

    function test_FreshAtExactlyMaxStaleness() public {
        wldFeed.setUpdatedAt(block.timestamp - MAX_STALENESS);
        oracle.wldForEth(1e18); // boundary is inclusive: still usable
    }

    function test_RevertWhen_WldFeedStale() public {
        wldFeed.setUpdatedAt(block.timestamp - MAX_STALENESS - 1);
        vm.expectRevert(
            abi.encodeWithSelector(
                ChainlinkWldEthOracle.StalePrice.selector, address(wldFeed), block.timestamp - MAX_STALENESS - 1
            )
        );
        oracle.wldForEth(1e18);
    }

    function test_RevertWhen_EthFeedStale() public {
        ethFeed.setUpdatedAt(block.timestamp - MAX_STALENESS - 1);
        vm.expectRevert();
        oracle.ethForWld(1e18);
    }

    function test_RevertWhen_RoundUnset() public {
        wldFeed.setUpdatedAt(0);
        vm.expectRevert(abi.encodeWithSelector(ChainlinkWldEthOracle.StalePrice.selector, address(wldFeed), 0));
        oracle.wldForEth(1e18);
    }

    function test_RevertWhen_PriceZeroOrNegative() public {
        wldFeed.setAnswer(0);
        vm.expectRevert(abi.encodeWithSelector(ChainlinkWldEthOracle.InvalidPrice.selector, address(wldFeed), 0));
        oracle.wldForEth(1e18);

        wldFeed.setAnswer(-1);
        vm.expectRevert(abi.encodeWithSelector(ChainlinkWldEthOracle.InvalidPrice.selector, address(wldFeed), -1));
        oracle.wldForEth(1e18);
    }

    /// @dev A reverting feed must propagate (fail-closed), never be swallowed.
    function test_RevertWhen_FeedReverts() public {
        ethFeed.setReverting(true);
        vm.expectRevert(bytes("feed down"));
        oracle.wldForEth(1e18);
    }

    function test_RevertWhen_AmountTooLarge() public {
        vm.expectRevert(ChainlinkWldEthOracle.AmountTooLarge.selector);
        oracle.wldForEth(uint256(type(uint128).max) + 1);
        vm.expectRevert(ChainlinkWldEthOracle.AmountTooLarge.selector);
        oracle.ethForWld(uint256(type(uint128).max) + 1);
    }

    function test_RevertWhen_BadConstructorArgs() public {
        vm.expectRevert(ChainlinkWldEthOracle.ZeroAddress.selector);
        new ChainlinkWldEthOracle(IAggregatorV3(address(0)), ethFeed, MAX_STALENESS);

        vm.expectRevert(ChainlinkWldEthOracle.InvalidStaleness.selector);
        new ChainlinkWldEthOracle(wldFeed, ethFeed, 0);

        MockAggregatorV3 tooManyDecimals = new MockAggregatorV3("WLD/USD", 19, WLD_USD);
        vm.expectRevert(
            abi.encodeWithSelector(
                ChainlinkWldEthOracle.UnsupportedDecimals.selector, address(tooManyDecimals), uint8(19)
            )
        );
        new ChainlinkWldEthOracle(tooManyDecimals, ethFeed, MAX_STALENESS);
    }
}
