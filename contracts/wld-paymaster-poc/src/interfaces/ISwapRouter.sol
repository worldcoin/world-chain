// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

/**
 * @title ISwapRouter (minimal)
 * @notice The subset of Uniswap's `SwapRouter02` used by the paymaster.
 * @dev Re-declared locally (instead of importing v3-periphery) to avoid the
 *      solc 0.7.6 pragma constraints of the upstream package.
 *
 *      IMPORTANT: this is the **SwapRouter02** (`IV3SwapRouter`) struct, which has
 *      **no `deadline` field** — selector `0x04e45aaf`. The original v3-periphery
 *      `SwapRouter` struct includes `deadline` and hashes to a different selector
 *      (`0x414bf389`). World Chain only has SwapRouter02 deployed
 *      (`0x091AD9e2e6e5eD44c1c66dB50e49A601F9f36cF6`); the legacy router is *not*
 *      there, so using the deadline variant makes every swap revert with no
 *      matching function. Do not "restore" the field.
 *
 *      SwapRouter02 drops `deadline` because deadline-checked calls are expected
 *      to go through its `multicall(uint256 deadline, bytes[])` wrapper. The
 *      paymaster does not need one: `triggerBatchSwap` is only reachable once per
 *      `blocksPerBatch` and its output is bounded by `amountOutMinimum`.
 */
interface ISwapRouter {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    /// @notice Swaps `amountIn` of one token for as much as possible of another token.
    function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);
}

/// @notice Minimal WETH9 interface for unwrapping swap proceeds to native ETH.
interface IWETH9 {
    function withdraw(uint256 wad) external;
    function balanceOf(address account) external view returns (uint256);
}
