// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

/**
 * @title IUniswapV3PoolMinimal
 * @notice The subset of the Uniswap V3 pool interface required to read a TWAP.
 */
interface IUniswapV3PoolMinimal {
    /// @notice Returns the cumulative tick and liquidity as of each `secondsAgos`.
    /// @param secondsAgos From how long ago each cumulative value should be returned.
    /// @return tickCumulatives Cumulative tick values as of each `secondsAgos`.
    /// @return secondsPerLiquidityCumulativeX128s Cumulative seconds-per-liquidity values.
    function observe(uint32[] calldata secondsAgos)
        external
        view
        returns (int56[] memory tickCumulatives, uint160[] memory secondsPerLiquidityCumulativeX128s);

    function token0() external view returns (address);
    function token1() external view returns (address);
    function fee() external view returns (uint24);
}
