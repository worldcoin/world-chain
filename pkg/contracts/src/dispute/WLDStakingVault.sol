// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {GameTypes} from "./lib/GameTypes.sol";
import {
    ChallengerBondAlreadyLocked,
    ExactTransferRequired,
    GameAlreadySettled,
    GameBondAlreadyInitialized,
    GameNotRegistered,
    InsufficientBalance,
    InvalidAccount,
    InvalidAmount,
    InvalidPayoutTotal,
    InvalidVaultConfiguration,
    InvalidWithdrawal,
    NotProxyAdminOwner,
    OwnerMismatch,
    UnexpectedGameAddress,
    WithdrawalDelayNotMet,
    WithdrawalPaused
} from "./lib/Errors.sol";
import {IMultiProofGame} from "./interfaces/IMultiProofGame.sol";
import {IWLDStakingVault} from "./interfaces/IWLDStakingVault.sol";

import {Claim, GameType, Hash} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";
import {IDisputeGameFactory} from "@optimism-bedrock/interfaces/dispute/IDisputeGameFactory.sol";
import {ISystemConfig} from "@optimism-bedrock/interfaces/L1/ISystemConfig.sol";
import {IProxyAdmin} from "@optimism-bedrock/interfaces/universal/IProxyAdmin.sol";
import {Initializable} from "@openzeppelin/contracts/proxy/utils/Initializable.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {LibClone} from "@solady/utils/LibClone.sol";

/// @custom:proxied true
/// @title WLDStakingVault
/// @notice A one-to-one WLD ledger for WIP-1006 proposal and challenge bonds.
contract WLDStakingVault is Initializable, IWLDStakingVault {
    using SafeERC20 for IERC20;

    /// @notice Semantic version.
    string public constant version = "1.0.0";

    uint256 internal immutable DELAY_SECONDS;

    IERC20 public wld;
    ISystemConfig public systemConfig;
    IDisputeGameFactory public disputeGameFactory;

    mapping(address account => uint256 amount) public availableBalance;
    mapping(address account => WithdrawalRequest request) public withdrawals;
    mapping(address game => GameBond bond) public gameBonds;

    constructor(uint256 delaySeconds) {
        DELAY_SECONDS = delaySeconds;
        _disableInitializers();
    }

    /// @inheritdoc IWLDStakingVault
    function initialize(IERC20 wld_, ISystemConfig systemConfig_, IDisputeGameFactory disputeGameFactory_)
        external
        initializer
    {
        _assertProxyAdminOrOwner();
        if (
            address(wld_) == address(0) || address(wld_).code.length == 0 || address(systemConfig_) == address(0)
                || address(systemConfig_).code.length == 0 || address(disputeGameFactory_) == address(0)
                || address(disputeGameFactory_).code.length == 0
        ) {
            revert InvalidVaultConfiguration();
        }
        wld = wld_;
        systemConfig = systemConfig_;
        disputeGameFactory = disputeGameFactory_;
    }

    /// @inheritdoc IWLDStakingVault
    function delay() external view returns (uint256) {
        return DELAY_SECONDS;
    }

    /// @inheritdoc IWLDStakingVault
    function proxyAdmin() public view returns (IProxyAdmin admin) {
        address adminAddress = ERC1967Utils.getAdmin();
        if (adminAddress == address(0)) revert InvalidVaultConfiguration();
        return IProxyAdmin(adminAddress);
    }

    /// @inheritdoc IWLDStakingVault
    function proxyAdminOwner() public view returns (address) {
        return proxyAdmin().owner();
    }

    /// @inheritdoc IWLDStakingVault
    function deposit(address account, uint256 amount) external {
        _deposit(account, amount);
    }

    /// @inheritdoc IWLDStakingVault
    function requestWithdrawal(uint256 amount) external {
        if (amount == 0) revert InvalidWithdrawal();
        uint256 available = availableBalance[msg.sender];
        if (available < amount) revert InsufficientBalance(msg.sender, available, amount);
        availableBalance[msg.sender] = available - amount;

        WithdrawalRequest storage request = withdrawals[msg.sender];
        request.amount += amount;
        request.timestamp = block.timestamp;
        emit WithdrawalRequested(msg.sender, request.amount, block.timestamp + DELAY_SECONDS);
    }

    /// @inheritdoc IWLDStakingVault
    function withdraw(uint256 amount) external {
        if (amount == 0) revert InvalidWithdrawal();
        if (systemConfig.paused()) revert WithdrawalPaused();

        WithdrawalRequest storage request = withdrawals[msg.sender];
        if (request.amount < amount || request.timestamp == 0) revert InvalidWithdrawal();
        uint256 availableAt = request.timestamp + DELAY_SECONDS;
        if (block.timestamp < availableAt) revert WithdrawalDelayNotMet(availableAt);

        request.amount -= amount;
        if (request.amount == 0) request.timestamp = 0;
        wld.safeTransfer(msg.sender, amount);
        emit Withdrawn(msg.sender, amount);
    }

    /// @inheritdoc IWLDStakingVault
    function lockProposerBond() external {
        _assertAlignedOwners();

        IMultiProofGame game = IMultiProofGame(msg.sender);
        if (
            GameType.unwrap(game.gameType()) != GameType.unwrap(GameTypes.MULTI_PROOF_GAME_TYPE)
                || address(game.disputeGameFactory()) != address(disputeGameFactory)
                || address(game.bondVault()) != address(this)
        ) {
            revert GameNotRegistered(msg.sender);
        }

        // The factory registers a game only after `initialize` returns, so the factory mapping
        // cannot authenticate this call yet. Instead recompute the deterministic clone address
        // the factory derives for the current implementation and the caller's creation data;
        // only a clone the factory itself deployed can occupy it.
        (GameType gameType_, Claim rootClaim_, bytes memory extraData_) = game.gameData();
        Hash uuid = disputeGameFactory.getGameUUID(gameType_, rootClaim_, extraData_);
        address implementation = address(disputeGameFactory.gameImpls(GameTypes.MULTI_PROOF_GAME_TYPE));
        address expectedGame = LibClone.predictDeterministicAddress(
            implementation,
            abi.encodePacked(game.gameCreator(), rootClaim_, game.l1Head(), extraData_),
            Hash.unwrap(uuid),
            address(disputeGameFactory)
        );
        if (expectedGame != msg.sender) revert UnexpectedGameAddress(expectedGame, msg.sender);
        if (gameBonds[msg.sender].proposerBond != 0) revert GameBondAlreadyInitialized(msg.sender);

        address proposer = game.gameCreator();
        uint256 amount = game.proposerBond();
        if (amount == 0) revert InvalidVaultConfiguration();
        _debitAvailable(proposer, amount);
        gameBonds[msg.sender] = GameBond({proposerBond: amount, challengerBond: 0, settled: false});
        emit ProposerBondLocked(msg.sender, proposer, amount);
    }

    /// @inheritdoc IWLDStakingVault
    function lockChallengerBond() external {
        _assertAlignedOwners();

        IMultiProofGame game = _registeredGame(msg.sender);
        GameBond storage bond = gameBonds[msg.sender];
        if (bond.proposerBond == 0) revert GameNotRegistered(msg.sender);
        if (bond.challengerBond != 0 || bond.settled) revert ChallengerBondAlreadyLocked(msg.sender);

        address challenger = game.challenger();
        uint256 amount = game.challengerBond();
        if (challenger == address(0) || amount == 0) revert InvalidVaultConfiguration();
        _debitAvailable(challenger, amount);
        bond.challengerBond = amount;
        emit ChallengerBondLocked(msg.sender, challenger, amount);
    }

    /// @inheritdoc IWLDStakingVault
    function settle(Payout[] calldata payouts) external {
        _registeredGame(msg.sender);
        GameBond storage bond = gameBonds[msg.sender];
        if (bond.proposerBond == 0) revert GameNotRegistered(msg.sender);
        if (bond.settled) revert GameAlreadySettled(msg.sender);

        uint256 payoutTotal;
        for (uint256 i; i < payouts.length; i++) {
            payoutTotal += payouts[i].amount;
        }

        uint256 expected = bond.proposerBond + bond.challengerBond;
        if (payoutTotal != expected) revert InvalidPayoutTotal(expected, payoutTotal);

        bond.settled = true;
        for (uint256 i; i < payouts.length; i++) {
            availableBalance[payouts[i].recipient] += payouts[i].amount;
        }
        emit GameSettled(msg.sender, payoutTotal);
    }

    /// @inheritdoc IWLDStakingVault
    function hold(address account) external {
        hold(account, availableBalance[account] + withdrawals[account].amount);
    }

    // TODO(security): Reassess unrestricted `hold` authority before production deployment. This
    // intentionally mirrors DelayedWETH's break-glass custody model, but this singleton holds
    // participants' settled WLD credits and therefore has a larger blast radius.
    /// @inheritdoc IWLDStakingVault
    function hold(address account, uint256 amount) public {
        address recipient = _assertProxyAdminOwner();
        uint256 heldBalance = availableBalance[account] + withdrawals[account].amount;
        if (heldBalance < amount) revert InsufficientBalance(account, heldBalance, amount);
        if (account == recipient || amount == 0) return;

        uint256 fromAvailable = amount < availableBalance[account] ? amount : availableBalance[account];
        availableBalance[account] -= fromAvailable;
        uint256 fromWithdrawal = amount - fromAvailable;
        if (fromWithdrawal != 0) {
            withdrawals[account].amount -= fromWithdrawal;
            if (withdrawals[account].amount == 0) withdrawals[account].timestamp = 0;
        }
        availableBalance[recipient] += amount;
        emit AccountHeld(account, recipient, amount);
    }

    function _deposit(address account, uint256 amount) internal {
        if (account == address(0)) revert InvalidAccount();
        if (amount == 0) revert InvalidAmount();

        uint256 balanceBefore = wld.balanceOf(address(this));
        wld.safeTransferFrom(msg.sender, address(this), amount);
        uint256 received = wld.balanceOf(address(this)) - balanceBefore;
        if (received != amount) revert ExactTransferRequired(amount, received);

        availableBalance[account] += amount;
        emit Deposited(msg.sender, account, amount);
    }

    function _registeredGame(address gameAddress) internal view returns (IMultiProofGame game) {
        game = IMultiProofGame(gameAddress);
        (GameType gameType_, Claim rootClaim_, bytes memory extraData_) = game.gameData();
        if (GameType.unwrap(gameType_) != GameType.unwrap(GameTypes.MULTI_PROOF_GAME_TYPE)) {
            revert GameNotRegistered(gameAddress);
        }
        (IDisputeGame registered,) = disputeGameFactory.games(gameType_, rootClaim_, extraData_);
        if (address(registered) != gameAddress || address(game.bondVault()) != address(this)) {
            revert GameNotRegistered(gameAddress);
        }
    }

    // Bond locking lets factory-selected game code allocate participant balances, so the factory
    // owner must be the same governance authority that controls this vault.
    function _assertAlignedOwners() internal view {
        address factoryOwner = disputeGameFactory.owner();
        address vaultOwner = proxyAdminOwner();
        if (factoryOwner != vaultOwner) revert OwnerMismatch(factoryOwner, vaultOwner);
    }

    function _assertProxyAdminOrOwner() internal view {
        IProxyAdmin admin = proxyAdmin();
        if (msg.sender != address(admin) && msg.sender != admin.owner()) revert NotProxyAdminOwner(msg.sender);
    }

    function _debitAvailable(address account, uint256 amount) internal {
        uint256 available = availableBalance[account];
        if (available < amount) revert InsufficientBalance(account, available, amount);
        availableBalance[account] = available - amount;
    }

    function _assertProxyAdminOwner() internal view returns (address owner) {
        owner = proxyAdminOwner();
        if (msg.sender != owner) revert NotProxyAdminOwner(msg.sender);
    }
}
