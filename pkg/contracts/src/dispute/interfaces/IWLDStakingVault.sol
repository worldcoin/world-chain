// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IDisputeGameFactory} from "@optimism-bedrock/interfaces/dispute/IDisputeGameFactory.sol";
import {ISystemConfig} from "@optimism-bedrock/interfaces/L1/ISystemConfig.sol";
import {IProxyAdmin} from "@optimism-bedrock/interfaces/universal/IProxyAdmin.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @title IWLDStakingVault
/// @notice Custodies WLD used by WIP-1006 proposers, challengers, and reward recipients.
interface IWLDStakingVault {
    struct GameBond {
        uint256 proposerBond;
        uint256 challengerBond;
        bool settled;
    }

    struct WithdrawalRequest {
        uint256 amount;
        uint256 timestamp;
    }

    struct Payout {
        address recipient;
        uint256 amount;
    }

    error ChallengerBondAlreadyPulled(address game);
    error ExactTransferRequired(uint256 expected, uint256 actual);
    error GameAlreadySettled(address game);
    error GameBondAlreadyInitialized(address game);
    error GameNotRegistered(address game);
    error InsufficientBalance(address account, uint256 available, uint256 required);
    error InvalidPayoutTotal(uint256 expected, uint256 actual);
    error InvalidVaultConfiguration();
    error InvalidWithdrawal();
    error NotProxyAdminOwner(address caller);
    error OwnerMismatch(address disputeGameFactoryOwner, address proxyAdminOwner);
    error UnexpectedGameAddress(address expected, address actual);
    error WithdrawalDelayNotMet(uint256 availableAt);
    error WithdrawalPaused();

    event ProposerBondPulled(address indexed game, address indexed proposer, uint256 amount);
    event ChallengerBondPulled(address indexed game, address indexed challenger, uint256 amount);
    event GameSettled(address indexed game, uint256 amount);
    event WithdrawalRequested(address indexed account, uint256 amount, uint256 availableAt);
    event Withdrawn(address indexed account, uint256 amount);
    event AccountHeld(address indexed account, address indexed recipient, uint256 amount);
    event Recovered(address indexed recipient, uint256 amount);

    /// @notice Initializes the proxy with its fixed external dependencies.
    function initialize(IERC20 wld, ISystemConfig systemConfig, IDisputeGameFactory disputeGameFactory) external;

    /// @notice Canonical WLD token backing every vault liability.
    function wld() external view returns (IERC20);

    /// @notice System configuration supplying the shared pause state.
    function systemConfig() external view returns (ISystemConfig);

    /// @notice Canonical factory allowed to create and register WIP-1006 games.
    function disputeGameFactory() external view returns (IDisputeGameFactory);

    /// @notice Delay between requesting and executing an external WLD withdrawal.
    function delay() external view returns (uint256);

    /// @notice Total WLD owed across available, pending, and active-game balances.
    function totalLiabilities() external view returns (uint256);

    /// @notice Settled WLD credit available to request for withdrawal.
    function availableBalance(address account) external view returns (uint256);

    /// @notice Pending external withdrawal amount and the timestamp of its latest request.
    function withdrawals(address account) external view returns (uint256 amount, uint256 timestamp);

    /// @notice Bond amounts assigned to a game and whether its complete pot was settled.
    function gameBonds(address game) external view returns (uint256 proposerBond, uint256 challengerBond, bool settled);

    /// @notice Whether backing WLD currently covers every recorded liability.
    function isSolvent() external view returns (bool);

    /// @notice ProxyAdmin stored in the OP Proxy's ERC-1967 admin slot.
    function proxyAdmin() external view returns (IProxyAdmin);

    /// @notice Owner of the vault's ProxyAdmin and break-glass custody authority.
    function proxyAdminOwner() external view returns (address);

    /// @notice Moves available WLD into a pending withdrawal and resets its full delay.
    function requestWithdrawal(uint256 amount) external;

    /// @notice Transfers matured pending WLD to the caller while the system is unpaused.
    function withdraw(uint256 amount) external;

    /// @notice Pulls the calling game's proposer bond from its creator's WLD allowance.
    /// @dev Called during `initialize`, before the factory registers the game, so the caller is
    ///      authenticated as the deterministic clone the factory deploys for its creation data.
    function pullProposerBond() external;

    /// @notice Pulls the calling registered game's single challenger bond from the challenger's
    ///         WLD allowance.
    function pullChallengerBond() external;

    /// @notice Credits a registered game's complete finalized pot exactly once.
    function settle(Payout[] calldata payouts) external;

    /// @notice Moves all of an account's available and pending claims to the ProxyAdmin owner.
    function hold(address account) external;

    /// @notice Moves an amount of an account's available and pending claims to the ProxyAdmin owner.
    function hold(address account, uint256 amount) external;

    /// @notice Transfers backing WLD to the ProxyAdmin owner without reducing liabilities.
    function recover(uint256 amount) external;
}
