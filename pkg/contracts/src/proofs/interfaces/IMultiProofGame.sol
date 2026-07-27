// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ProofLib} from "../lib/ProofLib.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";
import {IDisputeGameFactory} from "@optimism-bedrock/interfaces/dispute/IDisputeGameFactory.sol";
import {IAnchorStateRegistry} from "@optimism-bedrock/interfaces/dispute/IAnchorStateRegistry.sol";

/// @notice The WIP-1006 proof-lane extensions layered on top of the stock `IDisputeGame`
///         surface. Only the members `IDisputeGame` does not already declare live here;
///         proof-lane verifiers and offchain services need them to bind a proof to a game.
interface IMultiProofGame is IDisputeGame {
    function rootId() external view returns (bytes32);
    function anchorStateRegistry() external view returns (IAnchorStateRegistry);
    function disputeGameFactory() external view returns (IDisputeGameFactory);
    function domain() external view returns (ProofLib.Domain memory);
    function domainHash() external view returns (bytes32);
    function proposalDomainHash() external view returns (bytes32);
    function attempt() external view returns (uint256);
    function parentRef() external view returns (address);
    function startingRootClaim() external view returns (bytes32);
    function startingL2BlockNumber() external view returns (uint256);
    function l2BlockNumber() external view returns (uint256);
    function l1OriginHash() external view returns (bytes32);
    function l1OriginNumber() external view returns (uint256);
    function state() external view returns (ProofLib.RootState);
    function invalidationReason() external view returns (ProofLib.InvalidationReason);
    function proofBitmap() external view returns (uint8);
    function proofDeadline() external view returns (uint64);
    function challengeDeadline() external view returns (uint64);
    function challengedAt() external view returns (uint64);
    function finalizedAt() external view returns (uint64);
    function resolutionStatus()
        external
        view
        returns (bool resolvable, ProofLib.RootState outcome, ProofLib.InvalidationReason reason);
    /// @notice Finalizes bond distribution after the registry's finality airgap and attempts
    ///         to advance the anchor to this game.
    function closeGame() external;
    /// @notice Returns the credit `recipient` can claim from this game.
    function credit(address recipient) external view returns (uint256);
    /// @notice Permissionlessly claims `recipient`'s credit via the two-phase DelayedWETH flow;
    ///         the caller cannot redirect funds away from `recipient`.
    function claimCredit(address recipient) external;
}
