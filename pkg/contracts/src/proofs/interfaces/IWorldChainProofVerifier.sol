// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

interface IWorldChainProofVerifier {
    function verify(bytes32 rootId, bytes calldata proof) external view returns (bool);
}
