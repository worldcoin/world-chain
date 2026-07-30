// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

/**
 * @title ISwapRouter (minimal)
 * @notice The subset of the Uniswap V3 `SwapRouter` used by the paymaster.
 * @dev Re-declared locally (instead of importing v3-periphery) to avoid the
 *      solc 0.7.6 pragma constraints of the upstream package.
 */
interface ISwapRouter {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 deadline;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    /// @notice Swaps `amountIn` of one token for as much as possible of another token.
    function exactInputSingle(ExactInputSingleParams calldata params)
        external
        payable
        returns (uint256 amountOut);
}

/// @notice Minimal WETH9 interface for unwrapping swap proceeds to native ETH.
interface IWETH9 {
    function withdraw(uint256 wad) external;
    function balanceOf(address account) external view returns (uint256);
}
