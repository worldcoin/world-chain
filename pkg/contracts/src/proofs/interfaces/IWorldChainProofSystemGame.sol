// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {WorldChainProofLib} from "../WorldChainProofLib.sol";

interface IWorldChainProofSystemGame {
    function rootId() external view returns (bytes32);
    function factory() external view returns (address);
    function anchorStateRegistry() external view returns (address);
    function domainHash() external view returns (bytes32);
    function attempt() external view returns (uint256);
    function parentRef() external view returns (address);
    function startingRootClaim() external view returns (bytes32);
    function startingL2BlockNumber() external view returns (uint256);
    function rootClaim() external view returns (bytes32);
    function l2BlockNumber() external view returns (uint256);
    function l1OriginHash() external view returns (bytes32);
    function l1OriginNumber() external view returns (uint256);
    function state() external view returns (WorldChainProofLib.RootState);
    function invalidationReason() external view returns (WorldChainProofLib.InvalidationReason);
    function proofBitmap() external view returns (uint8);
    function proofDeadline() external view returns (uint64);
    function challengeDeadline() external view returns (uint64);
    function finalizedAt() external view returns (uint64);
    function resolutionStatus()
        external
        view
        returns (bool resolvable, WorldChainProofLib.RootState outcome, WorldChainProofLib.InvalidationReason reason);
    function resolve()
        external
        returns (WorldChainProofLib.RootState outcome, WorldChainProofLib.InvalidationReason reason);
}
