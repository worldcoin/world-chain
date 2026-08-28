// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IDisputeGameFactory} from "@optimism-bedrock/interfaces/dispute/IDisputeGameFactory.sol";
import {ISystemConfig} from "@optimism-bedrock/interfaces/L1/ISystemConfig.sol";
import {IProxyAdmin} from "@optimism-bedrock/interfaces/universal/IProxyAdmin.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @title IWLDStakingVault
/// @notice Custodies WLD used by WIP-1006 proposers, challengers, and reward recipients.
interface IWLDStakingVault {
    /// @notice The bonds a registered game custodies and whether its pot has been distributed.
    /// @param proposerBond The proposer bond locked when the game was created; non-zero marks the
    ///        game as registered.
    /// @param challengerBond The challenger bond, non-zero once a challenge has been locked.
    /// @param settled Whether the game's full pot has already been settled.
    struct GameBond {
        uint256 proposerBond;
        uint256 challengerBond;
        bool settled;
    }

    /// @notice An account's pending external withdrawal.
    /// @param amount The total WLD queued for withdrawal.
    /// @param timestamp The latest request time; the delay is measured from here, so a new
    ///        request restarts the wait for the whole pending amount.
    struct WithdrawalRequest {
        uint256 amount;
        uint256 timestamp;
    }

    /// @notice A single credit within a game settlement.
    /// @param recipient The account whose available balance is credited.
    /// @param amount The WLD credited to the recipient.
    struct Payout {
        address recipient;
        uint256 amount;
    }

    /// @notice Emitted when WLD is deposited into an account's available balance.
    /// @param depositor The account that supplied the WLD.
    /// @param account The account credited with the balance.
    /// @param amount The deposited WLD.
    event Deposited(address indexed depositor, address indexed account, uint256 amount);

    /// @notice Emitted when a game's proposer bond is locked from its creator's balance.
    /// @param game The game the bond is held against.
    /// @param proposer The account whose balance backed the bond.
    /// @param amount The locked proposer bond.
    event ProposerBondLocked(address indexed game, address indexed proposer, uint256 amount);

    /// @notice Emitted when a game's challenger bond is locked from the challenger's balance.
    /// @param game The game the bond is held against.
    /// @param challenger The account whose balance backed the bond.
    /// @param amount The locked challenger bond.
    event ChallengerBondLocked(address indexed game, address indexed challenger, uint256 amount);

    /// @notice Emitted when a game's pot is settled and distributed to payout recipients.
    /// @param game The settled game.
    /// @param amount The total pot distributed.
    event GameSettled(address indexed game, uint256 amount);

    /// @notice Emitted when an account moves available WLD into a pending withdrawal.
    /// @param account The requesting account.
    /// @param amount The total pending withdrawal after the request.
    /// @param availableAt The timestamp at which the pending amount becomes claimable.
    event WithdrawalRequested(address indexed account, uint256 amount, uint256 availableAt);

    /// @notice Emitted when an account claims a matured withdrawal.
    /// @param account The withdrawing account.
    /// @param amount The WLD transferred out.
    event Withdrawn(address indexed account, uint256 amount);

    /// @notice Emitted when an account's balances are seized to the ProxyAdmin owner via the
    ///         break-glass `hold` path.
    /// @param account The account whose balances were seized.
    /// @param recipient The ProxyAdmin owner credited with the seized balances.
    /// @param amount The seized WLD.
    event AccountHeld(address indexed account, address indexed recipient, uint256 amount);

    /// @notice Emitted when backing WLD is recovered to the ProxyAdmin owner without reducing
    ///         internal account or game balances.
    /// @param recipient The ProxyAdmin owner the WLD was transferred to.
    /// @param amount The recovered WLD.
    event Recovered(address indexed recipient, uint256 amount);

    /// @notice Initializes the proxy with its fixed external dependencies.
    function initialize(IERC20 wld, ISystemConfig systemConfig, IDisputeGameFactory disputeGameFactory) external;

    /// @notice Canonical WLD token held by the vault.
    function wld() external view returns (IERC20);

    /// @notice System configuration supplying the shared pause state.
    function systemConfig() external view returns (ISystemConfig);

    /// @notice Canonical factory allowed to create and register WIP-1006 games.
    function disputeGameFactory() external view returns (IDisputeGameFactory);

    /// @notice Delay between requesting and executing an external WLD withdrawal.
    function delay() external view returns (uint256);

    /// @notice Deposited or settled WLD available to fund bonds or request for withdrawal.
    function availableBalance(address account) external view returns (uint256);

    /// @notice Pending external withdrawal amount and the timestamp of its latest request.
    function withdrawals(address account) external view returns (uint256 amount, uint256 timestamp);

    /// @notice Bond amounts assigned to a game and whether its complete pot was settled.
    function gameBonds(address game) external view returns (uint256 proposerBond, uint256 challengerBond, bool settled);

    /// @notice ProxyAdmin stored in the OP Proxy's ERC-1967 admin slot.
    function proxyAdmin() external view returns (IProxyAdmin);

    /// @notice Owner of the vault's ProxyAdmin and break-glass custody authority.
    function proxyAdminOwner() external view returns (address);

    /// @notice Deposits the caller's WLD into an account's available balance.
    function deposit(address account, uint256 amount) external;

    /// @notice Moves available WLD into a pending withdrawal and resets its full delay.
    function requestWithdrawal(uint256 amount) external;

    /// @notice Transfers matured pending WLD to the caller while the system is unpaused.
    function withdraw(uint256 amount) external;

    /// @notice Locks the calling game's proposer bond from its creator's available balance.
    /// @dev Called during `initialize`, before the factory registers the game, so the caller is
    ///      authenticated as the deterministic clone the factory deploys for its creation data.
    function lockProposerBond() external;

    /// @notice Locks the calling registered game's single challenger bond from the challenger's
    ///         available balance.
    function lockChallengerBond() external;

    /// @notice Credits a registered game's complete finalized pot exactly once.
    function settle(Payout[] calldata payouts) external;

    /// @notice Moves all of an account's available and pending claims to the ProxyAdmin owner.
    function hold(address account) external;

    /// @notice Moves an amount of an account's available and pending claims to the ProxyAdmin owner.
    function hold(address account, uint256 amount) external;

    /// @notice Transfers backing WLD to the ProxyAdmin owner without reducing internal balances.
    function recover(uint256 amount) external;
}
