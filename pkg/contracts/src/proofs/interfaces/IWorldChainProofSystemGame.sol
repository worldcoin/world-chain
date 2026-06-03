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
    function parentRef() external view returns (address);
    function rootClaim() external view returns (bytes32);
    function l2BlockNumber() external view returns (uint256);
    function state() external view returns (RootState);
    function proofBitmap() external view returns (uint8);
    function proofDeadline() external view returns (uint64);
    function challengeDeadline() external view returns (uint64);
    function finalizedAt() external view returns (uint64);
}
