// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title IWorldChainStakingRegistry
/// @notice World Chain-specific challenger eligibility and `CHALLENGER_BOND`
///         escrow. No Base/OP analogue: the challenger side of WIP-1006 is
///         entirely World Chain-defined.
/// @dev Bond model is lock/refund/forfeit only. The registry never slashes the
///      challenger's underlying stake; it escrows the per-challenge bond paid
///      in by the game and routes it on resolution.
interface IWorldChainStakingRegistry {
    /// @notice Whether `account` is currently staked and eligible to challenge.
    function isStaked(address account) external view returns (bool);

    /// @notice Locks a `CHALLENGER_BOND` for `challenger` against `game`.
    /// @dev Called by the game on challenge with the bond forwarded as `msg.value`.
    ///      MUST verify the challenger is staked.
    function lockChallengerBond(address game, address challenger) external payable;

    /// @notice Refunds the locked challenger bond when `game` is invalidated.
    /// @dev Called by the game; transfers the bond back to `challenger`.
    function refundChallengerBond(address game, address challenger) external;

    /// @notice Forfeits the locked challenger bond when `game` finalizes.
    /// @dev Called by the game; the bond is retained by the registry.
    function forfeitChallengerBond(address game, address challenger) external;

    /// @notice The required challenger bond amount.
    function challengerBond() external view returns (uint256);
}
