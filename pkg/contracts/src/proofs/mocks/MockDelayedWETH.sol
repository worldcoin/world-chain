// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IDelayedWETH} from "../external/IDelayedWETH.sol";

/// @title MockDelayedWETH
/// @notice Test/devnet double for OP DelayedWETH. Escrows deposited ETH and
///         enforces a configurable withdrawal delay between `unlock` and
///         `withdraw`, mirroring the two-phase payout the game relies on.
contract MockDelayedWETH is IDelayedWETH {
    uint256 public delay;

    mapping(address guy => uint256 balance) public balances;
    mapping(address owner => mapping(address guy => uint256 amount)) public withdrawalAmount;
    mapping(address owner => mapping(address guy => uint256 timestamp)) public withdrawalTimestamp;

    error WithdrawalNotUnlocked();
    error WithdrawalDelayNotElapsed();
    error InsufficientBalance();
    error TransferFailed();

    constructor(uint256 delay_) {
        delay = delay_;
    }

    function setDelay(uint256 delay_) external {
        delay = delay_;
    }

    function deposit() external payable {
        balances[msg.sender] += msg.value;
    }

    function balanceOf(address guy) external view returns (uint256) {
        return balances[guy];
    }

    function hold(address guy, uint256 amount) external {
        balances[guy] += amount;
        balances[msg.sender] -= amount;
    }

    function hold(address guy) external {
        uint256 amount = balances[msg.sender];
        balances[guy] += amount;
        balances[msg.sender] = 0;
    }

    function unlock(address guy, uint256 amount) external {
        withdrawalAmount[msg.sender][guy] = amount;
        withdrawalTimestamp[msg.sender][guy] = block.timestamp;
    }

    function withdrawals(address owner, address guy) external view returns (uint256, uint256) {
        return (withdrawalAmount[owner][guy], withdrawalTimestamp[owner][guy]);
    }

    function withdraw(address guy, uint256 amount) public {
        if (withdrawalAmount[msg.sender][guy] < amount) revert WithdrawalNotUnlocked();
        if (block.timestamp < withdrawalTimestamp[msg.sender][guy] + delay) revert WithdrawalDelayNotElapsed();
        if (balances[guy] < amount) revert InsufficientBalance();

        withdrawalAmount[msg.sender][guy] -= amount;
        balances[guy] -= amount;
        (bool ok,) = msg.sender.call{value: amount}("");
        if (!ok) revert TransferFailed();
    }

    function withdraw(uint256 amount) external {
        withdraw(msg.sender, amount);
    }
}
