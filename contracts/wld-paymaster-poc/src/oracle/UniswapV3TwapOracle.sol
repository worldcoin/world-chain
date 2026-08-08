// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {IWldEthOracle} from "../interfaces/IWldEthOracle.sol";
import {OracleLibrary} from "../vendor/OracleLibrary.sol";

/**
 * @title UniswapV3TwapOracle
 * @notice {IWldEthOracle} implementation backed by a Uniswap V3 WLD/WETH pool TWAP.
 *
 * @dev Reads a time-weighted average tick over `twapWindow` seconds using the
 *      pool's built-in oracle (`observe`) and converts token amounts with
 *      {OracleLibrary.getQuoteAtTick}. Because the price is time-weighted, a
 *      single-block price spike must be sustained for the whole window to move
 *      the reported price meaningfully — this is the primary manipulation
 *      mitigation (see design doc).
 *
 *      If the pool has fewer than `twapWindow` seconds of observation history,
 *      `observe` reverts with "OLD"; that revert propagates and causes the
 *      paymaster to reject the UserOperation (fail-safe).
 *
 *      This contract is deliberately stateless/immutable so it can be shared and
 *      is trivially replaceable behind {IWldEthOracle} (e.g. by a Chainlink
 *      adapter) without changing the paymaster.
 */
contract UniswapV3TwapOracle is IWldEthOracle {
    /// @notice The Uniswap V3 pool observed (must contain both WLD and WETH).
    address public immutable pool;
    /// @notice The WLD token address.
    address public immutable wld;
    /// @notice The WETH token address (proxy for ETH price).
    address public immutable weth;
    /// @notice TWAP window in seconds.
    uint32 public immutable twapWindow;

    error AmountTooLarge();

    constructor(address _pool, address _wld, address _weth, uint32 _twapWindow) {
        require(_pool != address(0) && _wld != address(0) && _weth != address(0), "zero addr");
        require(_twapWindow > 0, "window=0");
        pool = _pool;
        wld = _wld;
        weth = _weth;
        twapWindow = _twapWindow;
    }

    /// @inheritdoc IWldEthOracle
    function wldForEth(uint256 ethWei) external view override returns (uint256 wldAmount) {
        if (ethWei > type(uint128).max) revert AmountTooLarge();
        int24 tick = OracleLibrary.consult(pool, twapWindow);
        // base = WETH amount (ethWei), quote = WLD
        wldAmount = OracleLibrary.getQuoteAtTick(tick, uint128(ethWei), weth, wld);
    }

    /// @inheritdoc IWldEthOracle
    function ethForWld(uint256 wldAmount) external view override returns (uint256 ethWei) {
        if (wldAmount > type(uint128).max) revert AmountTooLarge();
        int24 tick = OracleLibrary.consult(pool, twapWindow);
        // base = WLD amount, quote = WETH
        ethWei = OracleLibrary.getQuoteAtTick(tick, uint128(wldAmount), wld, weth);
    }
}
