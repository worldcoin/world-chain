// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "../../src/proofs/interfaces/IWorldChainProofVerifier.sol";
import {ProofLib} from "../../src/proofs/ProofLib.sol";

contract MockProofSystemGame {
    struct Context {
        ProofLib.Domain domain;
        bytes32 rootId;
        address anchorStateRegistry;
        bytes32 domainHash;
        address parentRef;
        bytes32 startingRootClaim;
        uint256 startingL2BlockNumber;
        bytes32 rootClaim;
        uint256 l2SequenceNumber;
        bytes32 l1Head;
        uint256 l1OriginNumber;
    }

    ProofLib.Domain internal _domain;
    bytes32 public rootId;
    address public anchorStateRegistry;
    bytes32 public domainHash;
    address public parentRef;
    bytes32 public startingRootClaim;
    uint256 public startingL2BlockNumber;
    bytes32 public rootClaim;
    uint256 public l2SequenceNumber;
    bytes32 public l1Head;
    uint256 public l1OriginNumber;

    function setContext(Context memory context) external {
        _domain = context.domain;
        rootId = context.rootId;
        anchorStateRegistry = context.anchorStateRegistry;
        domainHash = context.domainHash;
        parentRef = context.parentRef;
        startingRootClaim = context.startingRootClaim;
        startingL2BlockNumber = context.startingL2BlockNumber;
        rootClaim = context.rootClaim;
        l2SequenceNumber = context.l2SequenceNumber;
        l1Head = context.l1Head;
        l1OriginNumber = context.l1OriginNumber;
    }

    function domain() external view returns (ProofLib.Domain memory) {
        return _domain;
    }

    function verify(address verifier, bytes32 rootId_, bytes calldata proof) external view returns (bool) {
        return IWorldChainProofVerifier(verifier).verify(rootId_, proof);
    }
}
