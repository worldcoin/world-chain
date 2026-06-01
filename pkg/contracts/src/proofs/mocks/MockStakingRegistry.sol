// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainStakingRegistry} from "../interfaces/IWorldChainStakingRegistry.sol";

/// @title MockStakingRegistry
/// @notice Test/devnet double for the World Chain challenger staking registry.
///         Implements the lock/refund/forfeit bond model: a staked account may
///         challenge, the game escrows the bond here, and the game later refunds
///         (invalidated) or forfeits (finalized) it.
contract MockStakingRegistry is IWorldChainStakingRegistry {
    uint256 public challengerBond;

    mapping(address account => bool staked) public staked;
    mapping(address game => mapping(address challenger => uint256 amount)) public lockedBonds;
    uint256 public forfeitedBalance;

    error NotStaked(address account);
    error InvalidBond(uint256 expected, uint256 actual);
    error NoLockedBond(address game, address challenger);
    error TransferFailed();

    constructor(uint256 challengerBond_) {
        challengerBond = challengerBond_;
    }

    function setStaked(address account, bool staked_) external {
        staked[account] = staked_;
    }

    function isStaked(address account) external view returns (bool) {
        return staked[account];
    }

    function lockChallengerBond(address game, address challenger) external payable {
        if (!staked[challenger]) revert NotStaked(challenger);
        if (msg.value != challengerBond) revert InvalidBond(challengerBond, msg.value);
        lockedBonds[game][challenger] += msg.value;
    }

    function refundChallengerBond(address game, address challenger) external {
        if (msg.sender != game) revert NoLockedBond(game, challenger);
        uint256 amount = lockedBonds[game][challenger];
        if (amount == 0) revert NoLockedBond(game, challenger);
        lockedBonds[game][challenger] = 0;
        (bool ok,) = challenger.call{value: amount}("");
        if (!ok) revert TransferFailed();
    }

    function forfeitChallengerBond(address game, address challenger) external {
        if (msg.sender != game) revert NoLockedBond(game, challenger);
        uint256 amount = lockedBonds[game][challenger];
        if (amount == 0) revert NoLockedBond(game, challenger);
        lockedBonds[game][challenger] = 0;
        forfeitedBalance += amount;
    }
}
