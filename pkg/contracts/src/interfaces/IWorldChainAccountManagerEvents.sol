// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @title IWorldChainAccountManagerEvents
/// @author 0xOsiris, World Contributors
/// @notice Events emitted by `WORLD_CHAIN_ACCOUNT_MANAGER`. Each successful state transition MUST
///         emit exactly one matching event in the same transaction that mutates state.
/// @custom:security-contact security@toolsforhumanity.com
interface IWorldChainAccountManagerEvents {
    /// @notice Emitted when a new World Chain account is created via `create`.
    /// @param account The deterministic address of the newly created account.
    /// @param adminVerifier The address of the immutable admin verifier implementation.
    /// @param adminHash `keccak256(abi.encode(admin))` bound into the account address derivation.
    /// @param accountSalt The salt folded into the account address derivation.
    /// @param keyRingHash `keccak256(abi.encode(initialSessionVerifiers))` for the initial key ring.
    event AccountCreated(
        address indexed account,
        address indexed adminVerifier,
        bytes32 indexed adminHash,
        bytes32 accountSalt,
        bytes32 keyRingHash
    );

    /// @notice Emitted when an account's key ring is replaced via `setKeyRing`.
    /// @param account The account whose key ring was replaced.
    /// @param previousKeyRingHash The account's `keyRingHash` immediately before the replacement.
    /// @param newKeyRingHash The account's `keyRingHash` immediately after the replacement.
    /// @param adminNonce The account's `adminNonce` after this replacement (incremented by one).
    event KeyRingSet(
        address indexed account, bytes32 indexed previousKeyRingHash, bytes32 indexed newKeyRingHash, uint64 adminNonce
    );
}
