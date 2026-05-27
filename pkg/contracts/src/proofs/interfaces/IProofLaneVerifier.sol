// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

interface IProofLaneVerifier {
    function verify(bytes32 rootId, bytes calldata proof) external view returns (bool);
    function verifyInvalidity(bytes32 rootId, bytes calldata proof) external view returns (bool);
}
