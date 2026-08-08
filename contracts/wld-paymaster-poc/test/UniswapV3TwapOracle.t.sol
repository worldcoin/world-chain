// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {Test} from "forge-std/Test.sol";
import {UniswapV3TwapOracle} from "../src/oracle/UniswapV3TwapOracle.sol";
import {MockUniswapV3Pool} from "./mocks/Mocks.sol";

contract UniswapV3TwapOracleTest is Test {
    address wld = address(uint160(0xA11CE));
    address weth = address(uint160(0xB0B));

    function _oracle(int24 tick) internal returns (UniswapV3TwapOracle o, MockUniswapV3Pool pool) {
        pool = new MockUniswapV3Pool(wld, weth, tick);
        o = new UniswapV3TwapOracle(address(pool), wld, weth, 600);
    }

    function test_TickZeroIsParity() public {
        (UniswapV3TwapOracle o,) = _oracle(0);
        // At tick 0 the pool price is exactly 1:1.
        assertEq(o.wldForEth(1e18), 1e18);
        assertEq(o.ethForWld(1e18), 1e18);
    }

    function test_RoundTripApprox() public {
        // Arbitrary tick; converting ETH->WLD->ETH should ~roundtrip.
        (UniswapV3TwapOracle o,) = _oracle(6931); // ~1.0001^6931 ≈ 2.0
        uint256 wldOut = o.wldForEth(1e18);
        uint256 back = o.ethForWld(wldOut);
        assertApproxEqRel(back, 1e18, 1e15); // within 0.1%
    }

    function test_RevertWhen_ObservationTooOld() public {
        (UniswapV3TwapOracle o, MockUniswapV3Pool pool) = _oracle(0);
        pool.setTooOld(true);
        vm.expectRevert(bytes("OLD"));
        o.wldForEth(1e18);
    }

    function test_RevertWhen_AmountTooLarge() public {
        (UniswapV3TwapOracle o,) = _oracle(0);
        vm.expectRevert(UniswapV3TwapOracle.AmountTooLarge.selector);
        o.wldForEth(uint256(type(uint128).max) + 1);
    }
}
