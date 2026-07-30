// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "../../src/proofs/interfaces/IWorldChainProofVerifier.sol";
import {ProofLib} from "../../src/proofs/lib/ProofLib.sol";

/// @dev Test-only stand-in for a lane verifier. Deliberately lives under `test/` so it cannot be
///      wired into a production `GameConfig`.
contract MockRootIdVerifier is IWorldChainProofVerifier {
    mapping(bytes32 rootId => bool accepted) public acceptedRoots;
    bool public acceptAny;
    /// @dev Simulates a key-registry or verifier-gateway outage.
    bool public unavailable;

    constructor(bool acceptAny_) {
        acceptAny = acceptAny_;
    }

    function setAcceptAny(bool acceptAny_) external {
        acceptAny = acceptAny_;
    }

    function setAccepted(bytes32 rootId, bool accepted) external {
        acceptedRoots[rootId] = accepted;
    }

    function setUnavailable(bool unavailable_) external {
        unavailable = unavailable_;
    }

    function verify(bytes32 rootId, bytes calldata proof) external view returns (ProofLib.VerificationStatus) {
        if (unavailable) return ProofLib.VerificationStatus.UNAVAILABLE;
        if (acceptAny || acceptedRoots[rootId]) return ProofLib.VerificationStatus.VALID;
        if (proof.length != 32) return ProofLib.VerificationStatus.MALFORMED;
        return abi.decode(proof, (bytes32)) == rootId
            ? ProofLib.VerificationStatus.VALID
            : ProofLib.VerificationStatus.BINDING_MISMATCH;
    }
}
