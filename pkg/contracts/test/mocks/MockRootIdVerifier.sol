// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "../../src/dispute/interfaces/IWorldChainProofVerifier.sol";

interface IRootIdSource {
    function rootId() external view returns (bytes32);
}

/// @title MockRootIdVerifier
/// @author World Contributors
/// @custom:security-contact security@toolsforhumanity.com
contract MockRootIdVerifier is IWorldChainProofVerifier {
    mapping(bytes32 rootId => bool accepted) public acceptedRoots;
    bool public acceptAny;
    bool public enforceParameters;
    bytes32 public expectedVerifierId;
    bytes32 public expectedPublicValuesHash;

    constructor(bool acceptAny_) {
        acceptAny = acceptAny_;
    }

    function setAcceptAny(bool acceptAny_) external {
        acceptAny = acceptAny_;
    }

    function setAccepted(bytes32 rootId, bool accepted) external {
        acceptedRoots[rootId] = accepted;
    }

    function setExpectedParameters(bytes32 verifierId, bytes calldata publicValues) external {
        enforceParameters = true;
        expectedVerifierId = verifierId;
        expectedPublicValuesHash = keccak256(publicValues);
    }

    function verify(bytes calldata proof, bytes32 verifierId, bytes calldata publicValues)
        external
        view
        returns (bool)
    {
        if (enforceParameters && verifierId != expectedVerifierId) return false;
        if (enforceParameters && keccak256(publicValues) != expectedPublicValuesHash) return false;
        if (acceptAny) return true;
        if (proof.length != 32) return false;
        bytes32 rootId = abi.decode(proof, (bytes32));
        try IRootIdSource(msg.sender).rootId() returns (bytes32 expectedRootId) {
            return rootId == expectedRootId || acceptedRoots[rootId];
        } catch {
            return acceptedRoots[rootId];
        }
    }
}
