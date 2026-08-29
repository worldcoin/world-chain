// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

////////////////////////////////////////////////////////////////
//                  `ERC20StakingVault` Errors                  //
////////////////////////////////////////////////////////////////

/// @notice Thrown when a game's single challenger bond has already been locked.
/// @param game The address of the game whose challenger bond is already locked.
error ChallengerBondAlreadyLocked(address game);

/// @notice Thrown when a deposit does not credit the vault by exactly the stated amount,
///         guarding against fee-on-transfer or rebasing token behavior.
/// @param expected The amount the caller asked to deposit.
/// @param actual The balance increase the vault actually observed.
error ExactTransferRequired(uint256 expected, uint256 actual);

/// @notice Thrown when a game's pot has already been settled.
/// @param game The address of the already-settled game.
error GameAlreadySettled(address game);

/// @notice Thrown when a game's bond record has already been initialized.
/// @param game The address of the game whose bond is already recorded.
error GameBondAlreadyInitialized(address game);

/// @notice Thrown when the caller is not a MultiProofGame registered with the factory and bound
///         to this vault.
/// @param game The address that failed the registration check.
error GameNotRegistered(address game);

/// @notice Thrown when an account's available balance cannot cover a debit.
/// @param account The account being debited.
/// @param available The account's available balance.
/// @param required The amount the operation required.
error InsufficientBalance(address account, uint256 available, uint256 required);

/// @notice Thrown when settlement payouts do not sum to the game's full pot.
/// @param expected The pot to distribute (proposer bond plus challenger bond).
/// @param actual The sum of the supplied payouts.
error InvalidPayoutTotal(uint256 expected, uint256 actual);

/// @notice Thrown when a deposit targets the zero address.
error InvalidAccount();

error InvalidAmount();

/// @notice Thrown when a required dependency address is unset, has no code, or is mismatched
///         relative to the vault's expected wiring.
error InvalidVaultConfiguration();

error InvalidWithdrawal();

/// @notice Thrown when the caller is neither the ERC-1967 ProxyAdmin nor its owner.
/// @param caller The unauthorized caller.
error NotProxyAdminOwner(address caller);

/// @notice Thrown when the dispute game factory owner and the vault's ProxyAdmin owner diverge,
///         which would split the authority the bond-locking flow relies on.
/// @param disputeGameFactoryOwner The current factory owner.
/// @param proxyAdminOwner The current ProxyAdmin owner.
error OwnerMismatch(address disputeGameFactoryOwner, address proxyAdminOwner);

/// @notice Thrown when a locking game's address does not match the deterministic clone address
///         the factory derives for its creation data.
/// @param expected The predicted deterministic clone address.
/// @param actual The address that actually called in.
error UnexpectedGameAddress(address expected, address actual);

/// @notice Thrown when a matured withdrawal is claimed before its delay has elapsed.
/// @param availableAt The timestamp at which the withdrawal becomes claimable.
error WithdrawalDelayNotMet(uint256 availableAt);

error WithdrawalPaused();
