// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @title WorldChainAccountConstants
/// @author Worldcoin
/// @notice Domain seeds, hard-coded WIP-1001 limits, and default activation parameters.
/// @dev Per-instance activation parameters whose final value is selected at fork-config time
///      (`EIP1271_VALIDATION_GAS_LIMIT`, the byte caps) are stored on the manager and may be
///      tuned by the owner until {WorldChainAccountManagerImplV1.freezeActivationParameters}
///      is called. The defaults in this library are used at deployment time.
/// @custom:security-contact security@toolsforhumanity.com
library WorldChainAccountConstants {
    ///////////////////////////////////////////////////////////////////////////////
    ///                           PROTOCOL CONSTANTS                            ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice EIP-2718 transaction type for World Chain account transactions.
    uint8 internal constant WORLD_TX_TYPE = 0x1D;

    /// @notice Maximum number of session verifiers permitted in a key ring.
    uint256 internal constant MAX_SESSION_VERIFIERS = 20;

    /// @notice Domain seed for account-address derivation.
    /// @dev `keccak256("WIP1001_ACCOUNT")`.
    bytes32 internal constant WORLD_CHAIN_ACCOUNT_DOMAIN = keccak256("WIP1001_ACCOUNT");

    /// @notice Domain seed for the admin operation hash signed at `create`.
    /// @dev `keccak256("WORLD_CHAIN_ACCOUNT_CREATE")`.
    bytes32 internal constant WORLD_CHAIN_ACCOUNT_CREATE_DOMAIN = keccak256("WORLD_CHAIN_ACCOUNT_CREATE");

    /// @notice Domain seed for the admin operation hash signed at `setKeyRing`.
    /// @dev `keccak256("WORLD_CHAIN_ACCOUNT_SET")`.
    bytes32 internal constant WORLD_CHAIN_ACCOUNT_SET_DOMAIN = keccak256("WORLD_CHAIN_ACCOUNT_SET");

    /// @notice EIP-1271 success magic value (`isValidSignature.selector`).
    bytes4 internal constant EIP1271_MAGIC_VALUE = 0x1626ba7e;

    /// @notice EIP-1271 explicit-failure sentinel returned by the router when a check fails.
    bytes4 internal constant EIP1271_FAILURE_VALUE = 0xffffffff;

    ///////////////////////////////////////////////////////////////////////////////
    ///                       ACTIVATION-PARAMETER DEFAULTS                     ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Default EIP-1271 validation gas forwarded to admin and session verifiers.
    /// @dev Final value is selected at fork-config time and may be tuned via
    ///      `setEip1271ValidationGasLimit` until `freezeActivationParameters` is called.
    uint64 internal constant DEFAULT_EIP1271_VALIDATION_GAS_LIMIT = 200_000;

    /// @notice Default cap on each `WorldChainAccountVerifier.installation`.
    uint32 internal constant DEFAULT_MAX_VERIFIER_INSTALL_DATA_BYTES = 4096;

    /// @notice Default cap on `adminAuthorization` calldata.
    uint32 internal constant DEFAULT_MAX_ADMIN_AUTHORIZATION_BYTES = 8192;
}
