// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @title World ID Account Delegate
/// @author Worldcoin
/// @notice EIP-7702 delegation target for World ID Accounts.
/// @dev All World ID Accounts set their code to point to this contract via the EIP-7702
///      delegation designator. Calls execute in the context of the delegating account.
///
///      The actual transaction authorization happens at the protocol level via the 0x6f
///      transaction type validation in the node. This contract serves as:
///        1. The EIP-7702 delegation target (the account's code pointer).
///        2. Future: batch execution, key management view functions, etc.
///
/// @custom:security-contact security@toolsforhumanity.com
contract WorldIDAccountDelegate {
    /// @notice Accepts arbitrary calls (future: batch execution, view helpers).
    fallback() external payable {}

    /// @notice Accepts plain ETH transfers.
    receive() external payable {}
}
