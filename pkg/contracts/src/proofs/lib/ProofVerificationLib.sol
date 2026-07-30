// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IMultiProofGame} from "../interfaces/IMultiProofGame.sol";
import {ProofLib} from "./ProofLib.sol";
import {Claim, Hash} from "@optimism-bedrock/src/dispute/lib/Types.sol";

library ProofVerificationLib {
    /// @dev Validates the proof against the game's root, domain, and creation-time transition
    ///      snapshot. Returns `VALID` or `BINDING_MISMATCH`; cryptographic verification is the
    ///      caller's job.
    function matchesGame(
        address gameAddress,
        bytes32 rootId,
        bytes32 proofDomainHash,
        address proofParentRef,
        uint256 proofL1OriginNumber,
        ProofLib.TransitionPublicValues memory transition
    ) internal view returns (ProofLib.VerificationStatus) {
        IMultiProofGame game = IMultiProofGame(gameAddress);
        if (!_matchesRoot(game, rootId, proofDomainHash, proofParentRef, proofL1OriginNumber, transition)) {
            return ProofLib.VerificationStatus.BINDING_MISMATCH;
        }

        if (!_matchesDomain(game, proofDomainHash, transition.rollupConfigHash, game.domain())) {
            return ProofLib.VerificationStatus.BINDING_MISMATCH;
        }

        return _matchesTransition(game, proofL1OriginNumber, transition)
            ? ProofLib.VerificationStatus.VALID
            : ProofLib.VerificationStatus.BINDING_MISMATCH;
    }

    /// @dev The reconstructed rootId now commits to the pre-state, so this check alone pins the
    ///      full transition. `_matchesTransition` remains as defence in depth against a game whose
    ///      stored snapshot disagrees with the rootId it reports.
    function _matchesRoot(
        IMultiProofGame game,
        bytes32 rootId,
        bytes32 proofDomainHash,
        address proofParentRef,
        uint256 proofL1OriginNumber,
        ProofLib.TransitionPublicValues memory transition
    ) private view returns (bool) {
        bytes32 reconstructedRootId = ProofLib.rootId(
            proofDomainHash,
            proofParentRef,
            transition.l2PreRoot,
            uint256(transition.l2PreBlockNumber),
            transition.l2PostRoot,
            uint256(transition.l2PostBlockNumber),
            transition.l1Head,
            proofL1OriginNumber
        );

        return reconstructedRootId == rootId && game.rootId() == rootId && game.parentRef() == proofParentRef;
    }

    function _matchesDomain(
        IMultiProofGame game,
        bytes32 proofDomainHash,
        bytes32 transitionRollupConfigHash,
        ProofLib.Domain memory gameDomain
    ) private view returns (bool) {
        return game.domainHash() == proofDomainHash && ProofLib.domainHash(gameDomain) == proofDomainHash
            && transitionRollupConfigHash == gameDomain.rollupConfigHash;
    }

    function _matchesTransition(
        IMultiProofGame game,
        uint256 proofL1OriginNumber,
        ProofLib.TransitionPublicValues memory transition
    ) private view returns (bool) {
        return game.startingRootClaim() == transition.l2PreRoot
            && game.startingL2BlockNumber() == uint256(transition.l2PreBlockNumber)
            && Claim.unwrap(game.rootClaim()) == transition.l2PostRoot
            && game.l2SequenceNumber() == uint256(transition.l2PostBlockNumber)
            && Hash.unwrap(game.l1Head()) == transition.l1Head && game.l1OriginNumber() == proofL1OriginNumber;
    }
}
