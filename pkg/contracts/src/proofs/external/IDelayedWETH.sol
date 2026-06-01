// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title IDelayedWETH
/// @notice ABI-matched reimplementation of the OP Stack `IDelayedWETH`
///         interface, pinned to `op-contracts/v3.0.0-rc.2`. DelayedWETH escrows
///         the proposer bond and enforces a withdrawal delay between `unlock`
///         and `withdraw`, giving a guardian time to intervene before payout.
interface IDelayedWETH {
    /// @notice Holds `_amount` of WETH on behalf of `_guy`, charged against the caller's balance.
    function hold(address _guy, uint256 _amount) external;

    /// @notice Holds the full balance of `_guy` on behalf of the caller.
    function hold(address _guy) external;

    /// @notice Starts the withdrawal timer for `_amount` held against `_guy`.
    function unlock(address _guy, uint256 _amount) external;

    /// @notice Withdraws `_amount` previously unlocked against `_guy` to the caller.
    function withdraw(address _guy, uint256 _amount) external;

    /// @notice Withdraws `_amount` previously unlocked against the caller, to the caller.
    function withdraw(uint256 _amount) external;

    /// @notice Deposits ETH and mints WETH to the caller.
    function deposit() external payable;

    /// @notice Returns the WETH balance of `_guy`.
    function balanceOf(address _guy) external view returns (uint256);

    /// @notice Returns the pending withdrawal `(amount, timestamp)` for `(_owner, _guy)`.
    function withdrawals(address _owner, address _guy) external view returns (uint256, uint256);
}
