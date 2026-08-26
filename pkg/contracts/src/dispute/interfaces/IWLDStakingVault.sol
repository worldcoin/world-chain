// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Claim, Hash} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";
import {IDisputeGameFactory} from "@optimism-bedrock/interfaces/dispute/IDisputeGameFactory.sol";
import {ISystemConfig} from "@optimism-bedrock/interfaces/L1/ISystemConfig.sol";
import {IProxyAdmin} from "@optimism-bedrock/interfaces/universal/IProxyAdmin.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @title IWLDStakingVault
/// @notice Custodies WLD used by WIP-1006 proposers, challengers, and reward recipients.
interface IWLDStakingVault {
    struct ProposalReservation {
        address proposer;
        address implementation;
        uint256 amount;
    }

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

    error AlreadyReserved(Hash uuid);
    error ExactTransferRequired(uint256 expected, uint256 actual);
    error FactoryBondMustBeZero(uint256 bond);
    error GameAlreadyCreated(address game);
    error GameAlreadySettled(address game);
    error GameBondAlreadyInitialized(address game);
    error ChallengeBondAlreadyReserved(address game);
    error GameNotRegistered(address game);
    error InsufficientBalance(address account, uint256 available, uint256 required);
    error InvalidVaultConfiguration();
    error OwnerMismatch(address disputeGameFactoryOwner, address proxyAdminOwner);
    error InvalidAmount();
    error InvalidPayoutTotal(uint256 expected, uint256 actual);
    error InvalidReservation(Hash uuid);
    error InvalidWithdrawal();
    error NoGameImplementation();
    error NotProxyAdminOwner(address caller);
    error NotReservedProposer(address caller, address proposer);
    error ReservationNotStale(Hash uuid);
    error StaleReservation(Hash uuid);
    error UnexpectedGameAddress(address expected, address actual);
    error WithdrawalDelayNotMet(uint256 availableAt);
    error WithdrawalPaused();

    event Deposited(address indexed account, uint256 amount);
    event ProposalReserved(
        Hash indexed uuid,
        address indexed proposer,
        address indexed implementation,
        Claim rootClaim,
        bytes extraData,
        uint256 amount
    );
    event ProposalReservationReleased(Hash indexed uuid, address indexed proposer, uint256 amount);
    event ProposalBondAssigned(Hash indexed uuid, address indexed game, address indexed proposer, uint256 amount);
    event ChallengeBondReserved(address indexed game, address indexed challenger, uint256 amount);
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

    /// @notice Total WLD owed across available, pending, reserved, and active-game balances.
    function totalLiabilities() external view returns (uint256);

    /// @notice WLD immediately reusable or available to request for withdrawal.
    function availableBalance(address account) external view returns (uint256);

    /// @notice Pending external withdrawal amount and the timestamp of its latest request.
    function withdrawals(address account) external view returns (uint256 amount, uint256 timestamp);

    /// @notice Reservation bound to a deterministic factory game UUID.
    function proposalReservations(Hash uuid)
        external
        view
        returns (address proposer, address implementation, uint256 amount);

    /// @notice Bond amounts assigned to a game and whether its complete pot was settled.
    function gameBonds(address game) external view returns (uint256 proposerBond, uint256 challengerBond, bool settled);

    /// @notice Whether backing WLD currently covers every recorded liability.
    function isSolvent() external view returns (bool);

    /// @notice ProxyAdmin stored in the OP Proxy's ERC-1967 admin slot.
    function proxyAdmin() external view returns (IProxyAdmin);

    /// @notice Owner of the vault's ProxyAdmin and break-glass custody authority.
    function proxyAdminOwner() external view returns (address);

    /// @notice Deposits exact-transfer WLD into the caller's available balance.
    function deposit(uint256 amount) external;

    /// @notice Moves available WLD into a pending withdrawal and resets its full delay.
    function requestWithdrawal(uint256 amount) external;

    /// @notice Transfers matured pending WLD to the caller while the system is unpaused.
    function withdraw(uint256 amount) external;

    /// @notice Reserves the current implementation's proposer bond for a factory UUID.
    function reserveProposal(Claim rootClaim, bytes calldata extraData) external returns (Hash uuid);

    /// @notice Reserves a proposal and creates its game atomically through the stock factory.
    function reserveAndCreate(Claim rootClaim, bytes calldata extraData) external returns (IDisputeGame game);

    /// @notice Releases the caller's reservation if its game has not been created.
    function cancelProposal(Claim rootClaim, bytes calldata extraData) external;

    /// @notice Permissionlessly releases an uncreated reservation after its implementation is replaced.
    function invalidateStaleProposal(Claim rootClaim, bytes calldata extraData) external;

    /// @notice Assigns a reservation to the authenticated deterministic clone during initialization.
    function consumeProposal() external returns (address proposer);

    /// @notice Reserves the calling registered game's single challenger bond.
    function reserveChallenge() external;

    /// @notice Credits a registered game's complete finalized pot exactly once.
    function settle(Payout[] calldata payouts) external;

    /// @notice Moves all of an account's available and pending claims to the ProxyAdmin owner.
    function hold(address account) external;

    /// @notice Moves an amount of an account's available and pending claims to the ProxyAdmin owner.
    function hold(address account, uint256 amount) external;

    /// @notice Transfers backing WLD to the ProxyAdmin owner without reducing liabilities.
    function recover(uint256 amount) external;
}
