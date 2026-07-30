// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ProofLib} from "../lib/ProofLib.sol";

interface IWorldChainProofVerifier {
    /// @notice Verifies `proof` against `rootId` for the calling game.
    /// @dev The calling game is the source of truth for the expected proposal transition.
    ///
    ///      Implementations MUST return a status rather than reverting, so that one lane's
    ///      dependency outage cannot make a game unusable. They MUST distinguish
    ///      `UNAVAILABLE` (a key registry or verifier gateway failed, so the proof is unjudged)
    ///      from `REJECTED` (the proof bound correctly and failed its cryptographic check).
    ///      Collapsing the two strands an operator during a live proof window: the first calls
    ///      for a retry or a lane switch, the second for a new proof.
    function verify(bytes32 rootId, bytes calldata proof) external view returns (ProofLib.VerificationStatus status);
}
