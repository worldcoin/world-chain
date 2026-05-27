// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {IProofLaneVerifier} from "../../../src/proofs/interfaces/IProofLaneVerifier.sol";

contract MockProofLaneVerifier is IProofLaneVerifier {
    mapping(bytes32 rootId => bool supported) public supportsRoot;
    mapping(bytes32 rootId => bool invalidatesRoot) public invalidatesRoot;

    function setSupportsRoot(bytes32 rootId, bool supported) external {
        supportsRoot[rootId] = supported;
    }

    function setInvalidatesRoot(bytes32 rootId, bool invalidates) external {
        invalidatesRoot[rootId] = invalidates;
    }

    function verify(bytes32 rootId, bytes calldata) external view returns (bool) {
        return supportsRoot[rootId];
    }

    function verifyInvalidity(bytes32 rootId, bytes calldata) external view returns (bool) {
        return invalidatesRoot[rootId];
    }
}
