// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

interface IWorldChainProofSystemGame {
    enum RootState {
        NONE,
        PROPOSED,
        CHALLENGED,
        FINALIZED,
        INVALIDATED
    }

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
    function state() external view returns (RootState);
    function proofBitmap() external view returns (uint8);
    function proofDeadline() external view returns (uint64);
    function challengeDeadline() external view returns (uint64);
    function finalizedAt() external view returns (uint64);
}
