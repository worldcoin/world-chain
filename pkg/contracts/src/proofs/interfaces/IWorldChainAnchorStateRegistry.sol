// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

interface IWorldChainAnchorStateRegistry {
    function currentRootClaim() external view returns (bytes32);
}
