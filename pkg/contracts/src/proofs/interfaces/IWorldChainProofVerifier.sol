// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title IWorldChainProofVerifier
/// @author World Contributors
/// @custom:security-contact security@toolsforhumanity.com
interface IWorldChainProofVerifier {
    /// @dev The calling game is the source of truth for the expected proposal transition.
    function verify(bytes32 rootId, bytes calldata proof) external view returns (bool);
}
