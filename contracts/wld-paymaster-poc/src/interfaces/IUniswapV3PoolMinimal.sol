// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

/**
 * @title IUniswapV3PoolMinimal
 * @notice The subset of the Uniswap V3 pool interface required to read the
 *         current spot price (used by the paymaster's swap deviation guard).
 */
interface IUniswapV3PoolMinimal {
    /// @notice The pool's current price and oracle state.
    /// @return sqrtPriceX96 sqrt(token1/token0 price) as a Q64.96 fixed point.
    function slot0()
        external
        view
        returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint16 observationIndex,
            uint16 observationCardinality,
            uint16 observationCardinalityNext,
            uint8 feeProtocol,
            bool unlocked
        );

    function token0() external view returns (address);
    function token1() external view returns (address);
    function fee() external view returns (uint24);
}
