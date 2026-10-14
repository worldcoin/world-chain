// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IERC165} from "@openzeppelin/contracts/utils/introspection/IERC165.sol";

/// @title IRpSigner
/// @author Worldcoin
/// @notice WIP-101 World ID RP request-authorization interface. Implementers are registered
///         as the `signer` for a given `rpId` in the World ID `RpRegistry` and consulted by
///         OPRF nodes via read-only `eth_call` before they contribute their OPRF share.
///
///         Implementations MUST be ERC-165 discoverable so OPRF nodes can detect them at
///         registry ingest.
///
/// @custom:security-contact security@toolsforhumanity.com
interface IRpSigner is IERC165 {
    /// @notice The RP request is not valid. `code` is implementation-defined and used only
    ///         to aid off-chain debugging — OPRF nodes treat any revert as rejection.
    error RpInvalidRequest(uint256 code);

    /// @notice Verifies a World ID proof request is authorized by the RP. The `rpId` is
    ///         implicit in the registration — `RpRegistry` points at this contract for one
    ///         specific `rpId`.
    /// @dev MUST return the bytes4 magic value `0x35dbc8de` (function selector for
    ///      `verifyRpRequest`) when the request is valid.
    /// @dev MUST NOT modify state.
    /// @param version Wire-format / signature version for the request envelope.
    /// @param nonce Per-request nonce. Replay protection is implementation-defined; stateless
    ///         signers MAY ignore.
    /// @param createdAt Request creation timestamp (seconds since unix epoch).
    /// @param expiresAt Request expiration timestamp (seconds since unix epoch).
    /// @param action Already-hashed action value (a field element).
    /// @param data Arbitrary application-specific payload. Implementations MAY require empty.
    /// @return magicValue `0x35dbc8de` on success; otherwise the call reverts.
    function verifyRpRequest(
        uint8 version,
        uint256 nonce,
        uint64 createdAt,
        uint64 expiresAt,
        uint256 action,
        bytes calldata data
    ) external view returns (bytes4 magicValue);
}
