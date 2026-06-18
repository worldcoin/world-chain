// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @title IActionVerifier
/// @author Worldcoin
/// @notice Minimal interface a contract implements to declare which World ID Uniqueness
///         `action` values it currently authorizes under `WORLD_CHAIN_RP_ID`. Consumed by
///         `WorldChainRpSigner.verifyRpRequest` to route OPRF authorization decisions across
///         an owner-managed list of verifiers (e.g. subsidy accounting, future rate-limit
///         contracts). Each verifier owns its own time / rotation / validity policy; the
///         signer is a thin aggregator.
/// @dev Implementations MUST use a distinct keccak preimage / domain tag when deriving
///      valid actions so action spaces do not collide across verifiers — a Uniqueness
///      `action` accepted by any registered verifier is authorized at the OPRF layer.
///
/// @custom:security-contact security@toolsforhumanity.com
interface IActionVerifier {
    /// @notice Whether `action` is currently authorized under this verifier's policy.
    /// @dev MUST be a `view` function with no side effects; called by OPRF nodes via
    ///      `eth_call` against the registered `RpRegistry.signer`.
    function isValidAction(uint256 action) external view returns (bool);
}
