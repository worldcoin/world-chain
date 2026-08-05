// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "../../src/proofs/interfaces/IWorldChainProofVerifier.sol";

contract MockProofSystemGame {
    struct Context {
        bytes32 rootId;
        address anchorStateRegistry;
        bytes32 domainHash;
        bytes32 rollupConfigHash;
        address parentRef;
        bytes32 startingRootHash;
        uint256 startingBlockNumber;
        bytes32 rootClaim;
        uint256 l2BlockNumber;
        bytes32 l1OriginHash;
        uint256 l1OriginNumber;
    }

    bytes32 public rootId;
    address public anchorStateRegistry;
    bytes32 public domainHash;
    bytes32 public rollupConfigHash;
    address public parentRef;
    bytes32 public startingRootHash;
    uint256 public startingBlockNumber;
    bytes32 public rootClaim;
    uint256 public l2BlockNumber;
    bytes32 public l1OriginHash;
    uint256 public l1OriginNumber;

    function setContext(Context memory context) external {
        rootId = context.rootId;
        anchorStateRegistry = context.anchorStateRegistry;
        domainHash = context.domainHash;
        rollupConfigHash = context.rollupConfigHash;
        parentRef = context.parentRef;
        startingRootHash = context.startingRootHash;
        startingBlockNumber = context.startingBlockNumber;
        rootClaim = context.rootClaim;
        l2BlockNumber = context.l2BlockNumber;
        l1OriginHash = context.l1OriginHash;
        l1OriginNumber = context.l1OriginNumber;
    }

    function l1Head() external view returns (bytes32) {
        return l1OriginHash;
    }

    function l2SequenceNumber() external view returns (uint256) {
        return l2BlockNumber;
    }

    function verify(address verifier, bytes32 rootId_, bytes calldata proof) external view returns (bool) {
        return IWorldChainProofVerifier(verifier).verify(rootId_, proof);
    }
}
