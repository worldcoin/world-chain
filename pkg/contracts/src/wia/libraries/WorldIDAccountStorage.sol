// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @title World ID Account Storage
/// @author Worldcoin
/// @notice Library for computing storage slots used by World ID Accounts.
/// @dev These deterministic slots are used by the precompile to write directly into
///      account storage. In the Solidity reference implementation, they serve as documentation
///      of the intended storage layout.
/// @custom:security-contact security@toolsforhumanity.com
library WorldIDAccountStorage {
    /// @notice Slot for the account generation counter.
    bytes32 constant GENERATION_SLOT = keccak256("worldchain.world_id_account.generation");

    /// @notice Slot for the number of authorized keys.
    bytes32 constant NUM_KEYS_SLOT = keccak256("worldchain.world_id_account.num_keys");

    /// @notice Slot for the account nullifier.
    bytes32 constant NULLIFIER_SLOT = keccak256("worldchain.world_id_account.nullifier");

    /// @notice Computes the storage slot for the i-th authorized key.
    /// @param i The index of the key.
    /// @return The storage slot for key at index `i`.
    function keySlot(uint256 i) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked("worldchain.world_id_account.key", i));
    }
}
