// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainStakingRegistry} from "./interfaces/IWorldChainStakingRegistry.sol";

/// @title WorldChainStakingRegistry
/// @notice Tracks challenger eligibility and escrows the WIP-1006
///         `CHALLENGER_BOND`. A staked account may challenge; the game forwards
///         the bond here on challenge, and the game later instructs the registry
///         to refund (root invalidated) or forfeit (root finalized) the bond.
/// @dev World Chain-specific; no Base analogue. Lock/refund/forfeit only — the
///      account's underlying stake is never slashed.
contract WorldChainStakingRegistry is IWorldChainStakingRegistry {
    /// @notice The required challenger bond amount, in wei.
    uint256 public immutable challengerBond;

    /// @notice The owner permitted to manage the staked set.
    address public owner;

    /// @notice Whether an account is staked and eligible to challenge.
    mapping(address account => bool staked) public staked;

    /// @notice Per-(game, challenger) escrowed bond amount.
    mapping(address game => mapping(address challenger => uint256 amount)) public lockedBonds;

    /// @notice Total bonds forfeited to the registry.
    uint256 public forfeitedBalance;

    error NotOwner();
    error NotStaked(address account);
    error InvalidBond(uint256 expected, uint256 actual);
    error NoLockedBond(address game, address challenger);
    error TransferFailed(address recipient, uint256 amount);

    event StakedSet(address indexed account, bool staked);
    event ChallengerBondLocked(address indexed game, address indexed challenger, uint256 amount);
    event ChallengerBondRefunded(address indexed game, address indexed challenger, uint256 amount);
    event ChallengerBondForfeited(address indexed game, address indexed challenger, uint256 amount);

    /// @param challengerBond_ The required challenger bond amount.
    constructor(uint256 challengerBond_) {
        owner = msg.sender;
        challengerBond = challengerBond_;
    }

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    /// @notice Transfers ownership of the registry.
    function transferOwnership(address nextOwner) external onlyOwner {
        owner = nextOwner;
    }

    /// @notice Sets the staked status of `account`.
    function setStaked(address account, bool staked_) external onlyOwner {
        staked[account] = staked_;
        emit StakedSet(account, staked_);
    }

    /// @inheritdoc IWorldChainStakingRegistry
    function isStaked(address account) external view returns (bool) {
        return staked[account];
    }

    /// @inheritdoc IWorldChainStakingRegistry
    function lockChallengerBond(address game, address challenger) external payable {
        if (!staked[challenger]) revert NotStaked(challenger);
        if (msg.value != challengerBond) revert InvalidBond(challengerBond, msg.value);
        lockedBonds[game][challenger] += msg.value;
        emit ChallengerBondLocked(game, challenger, msg.value);
    }

    /// @inheritdoc IWorldChainStakingRegistry
    /// @dev Only the game that locked the bond can release it (`msg.sender == game`).
    function refundChallengerBond(address game, address challenger) external {
        if (msg.sender != game) revert NoLockedBond(game, challenger);
        uint256 amount = lockedBonds[game][challenger];
        if (amount == 0) revert NoLockedBond(game, challenger);
        lockedBonds[game][challenger] = 0;
        _transfer(challenger, amount);
        emit ChallengerBondRefunded(game, challenger, amount);
    }

    /// @inheritdoc IWorldChainStakingRegistry
    /// @dev Only the game that locked the bond can release it (`msg.sender == game`).
    function forfeitChallengerBond(address game, address challenger) external {
        if (msg.sender != game) revert NoLockedBond(game, challenger);
        uint256 amount = lockedBonds[game][challenger];
        if (amount == 0) revert NoLockedBond(game, challenger);
        lockedBonds[game][challenger] = 0;
        forfeitedBalance += amount;
        emit ChallengerBondForfeited(game, challenger, amount);
    }

    function _transfer(address recipient, uint256 amount) internal {
        (bool ok,) = recipient.call{value: amount}("");
        if (!ok) revert TransferFailed(recipient, amount);
    }
}
