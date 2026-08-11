// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

/**
 * @title IWldEthOracle
 * @notice Minimal price-oracle abstraction used by the WLD paymaster.
 *
 * The interface is intentionally implementation-agnostic: the MVP ships a
 * Chainlink-backed implementation ({ChainlinkWldEthOracle}), but the paymaster
 * only depends on this interface so the oracle can be swapped without touching
 * the paymaster.
 *
 * Conventions:
 *  - "WLD" amounts are denominated in the WLD token's smallest unit (1e18).
 *  - "ETH"/gas amounts are denominated in wei.
 *
 * Implementations MUST revert if a reliable price cannot be produced (e.g. a
 * stale Chainlink round). Reverting here causes `validatePaymasterUserOp` to
 * reject the UserOperation, which is the safe default.
 */
interface IWldEthOracle {
    /// @notice Amount of WLD (1e18) required to be worth `ethWei` of ETH.
    /// @dev Used to price gas: given the ETH cost of an op, how much WLD to charge.
    function wldForEth(uint256 ethWei) external view returns (uint256 wldAmount);

    /// @notice ETH value (wei) of `wldAmount` WLD (1e18).
    /// @dev Used to bound batch-swap slippage: expected ETH out for a WLD input.
    function ethForWld(uint256 wldAmount) external view returns (uint256 ethWei);
}
