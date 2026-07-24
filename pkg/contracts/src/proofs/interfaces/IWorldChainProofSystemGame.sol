// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {WorldChainProofLib} from "../WorldChainProofLib.sol";
import {GameStatus, Hash} from "@optimism-bedrock/src/dispute/lib/Types.sol";

interface IWorldChainProofSystemGame {
    function rootId() external view returns (bytes32);
    function anchorStateRegistry() external view returns (address);
    function disputeGameFactory() external view returns (address);
    function domain() external view returns (WorldChainProofLib.Domain memory);
    function domainHash() external view returns (bytes32);
    function proposalDomainHash() external view returns (bytes32);
    function attempt() external view returns (uint256);
    function parentRef() external view returns (address);
    function startingRootClaim() external view returns (bytes32);
    function startingL2BlockNumber() external view returns (uint256);
    function rootClaim() external view returns (bytes32);
    function l1Head() external view returns (Hash);
    function l2SequenceNumber() external view returns (uint256);
    function l2BlockNumber() external view returns (uint256);
    function l1OriginHash() external view returns (bytes32);
    function l1OriginNumber() external view returns (uint256);
    function status() external view returns (GameStatus);
    function state() external view returns (WorldChainProofLib.RootState);
    function invalidationReason() external view returns (WorldChainProofLib.InvalidationReason);
    function proofBitmap() external view returns (uint8);
    function proofDeadline() external view returns (uint64);
    function challengeDeadline() external view returns (uint64);
    function challengedAt() external view returns (uint64);
    function finalizedAt() external view returns (uint64);
    function resolutionStatus()
        external
        view
        returns (bool resolvable, WorldChainProofLib.RootState outcome, WorldChainProofLib.InvalidationReason reason);
    function resolve() external returns (GameStatus status_);
    /// @notice Finalizes bond distribution after the registry's finality airgap and attempts
    ///         to advance the anchor to this game.
    function closeGame() external;
    /// @notice Returns the credit `recipient` can claim from this game.
    function credit(address recipient) external view returns (uint256);
    /// @notice Permissionlessly claims `recipient`'s credit via the two-phase DelayedWETH flow;
    ///         the caller cannot redirect funds away from `recipient`.
    function claimCredit(address recipient) external;
}
