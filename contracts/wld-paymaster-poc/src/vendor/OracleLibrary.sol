// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {IUniswapV3PoolMinimal} from "../interfaces/IUniswapV3PoolMinimal.sol";
import {TickMath} from "./TickMath.sol";
import {FullMath} from "./FullMath.sol";

/// @title Oracle library
/// @notice Provides functions to integrate with V3 pool oracle. Ported from
/// Uniswap V3 `OracleLibrary` to solidity ^0.8.
library OracleLibrary {
    /// @notice Calculates the time-weighted arithmetic mean tick over `secondsAgo`.
    /// @param pool Address of the Uniswap V3 pool to observe.
    /// @param secondsAgo Number of seconds in the past from which to calculate the TWAP.
    /// @return arithmeticMeanTick The arithmetic mean tick over `secondsAgo`.
    function consult(address pool, uint32 secondsAgo) internal view returns (int24 arithmeticMeanTick) {
        require(secondsAgo != 0, "BP");

        uint32[] memory secondsAgos = new uint32[](2);
        secondsAgos[0] = secondsAgo;
        secondsAgos[1] = 0;

        (int56[] memory tickCumulatives,) = IUniswapV3PoolMinimal(pool).observe(secondsAgos);
        int56 tickCumulativesDelta = tickCumulatives[1] - tickCumulatives[0];

        arithmeticMeanTick = int24(tickCumulativesDelta / int56(uint56(secondsAgo)));
        // Always round to negative infinity
        if (tickCumulativesDelta < 0 && (tickCumulativesDelta % int56(uint56(secondsAgo)) != 0)) {
            arithmeticMeanTick--;
        }
    }

    /// @notice Given a tick and a token amount, calculates the amount of token received in exchange.
    /// @param tick Tick value used to calculate the quote.
    /// @param baseAmount Amount of token to be converted.
    /// @param baseToken Address of an ERC20 token contract used as the baseAmount denomination.
    /// @param quoteToken Address of an ERC20 token contract used as the quoteAmount denomination.
    /// @return quoteAmount Amount of quoteToken received for baseAmount of baseToken.
    function getQuoteAtTick(int24 tick, uint128 baseAmount, address baseToken, address quoteToken)
        internal
        pure
        returns (uint256 quoteAmount)
    {
        uint160 sqrtRatioX96 = TickMath.getSqrtRatioAtTick(tick);

        // Calculate quoteAmount with better precision if it doesn't overflow when multiplied by itself
        if (sqrtRatioX96 <= type(uint128).max) {
            uint256 ratioX192 = uint256(sqrtRatioX96) * sqrtRatioX96;
            quoteAmount = baseToken < quoteToken
                ? FullMath.mulDiv(ratioX192, baseAmount, 1 << 192)
                : FullMath.mulDiv(1 << 192, baseAmount, ratioX192);
        } else {
            uint256 ratioX128 = FullMath.mulDiv(sqrtRatioX96, sqrtRatioX96, 1 << 64);
            quoteAmount = baseToken < quoteToken
                ? FullMath.mulDiv(ratioX128, baseAmount, 1 << 128)
                : FullMath.mulDiv(1 << 128, baseAmount, ratioX128);
        }
    }
}
