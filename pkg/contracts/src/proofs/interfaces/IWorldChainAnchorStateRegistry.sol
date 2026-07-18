// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

interface IWorldChainAnchorStateRegistry {
    function setAnchorState(address game) external;
    function isGameFinalized(address game) external view returns (bool);
    function isGameClaimValid(address game) external view returns (bool);
    function proofSystemFactory() external view returns (address);
    function finalityDelay() external view returns (uint64);
    function paused() external view returns (bool);
    function currentRootClaim() external view returns (bytes32);
    function currentL2BlockNumber() external view returns (uint256);
    function anchorGame() external view returns (address);
    function blacklistedGames(address game) external view returns (bool);
}
