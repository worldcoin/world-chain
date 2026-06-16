// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "../interfaces/IWorldChainProofVerifier.sol";

contract MockRootIdVerifier is IWorldChainProofVerifier {
    mapping(bytes32 rootId => bool accepted) public acceptedRoots;
    bool public acceptAny;

    constructor(bool acceptAny_) {
        acceptAny = acceptAny_;
    }

    function setAcceptAny(bool acceptAny_) external {
        acceptAny = acceptAny_;
    }

    function setAccepted(bytes32 rootId, bool accepted) external {
        acceptedRoots[rootId] = accepted;
    }

    function verify(bytes32 rootId, bytes calldata proof) external view returns (bool) {
        if (acceptAny || acceptedRoots[rootId]) return true;
        if (proof.length != 32) return false;
        return abi.decode(proof, (bytes32)) == rootId;
    }
}
