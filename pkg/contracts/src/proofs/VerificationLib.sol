// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ProofLib} from "./ProofLib.sol";
import {MultiProofGame} from "./MultiProofGame.sol";

import {Claim, Hash} from "@optimism-bedrock/src/dispute/lib/Types.sol";

library VerificationLib {
    /// @dev Validates the proof against the game's root, domain, and creation-time transition snapshot.
    function matchesGame(
        address gameAddress,
        bytes32 rootId,
        bytes32 proofDomainHash,
        address proofParentRef,
        uint256 proofL1OriginNumber,
        ProofLib.TransitionPublicValues memory transition
    ) internal view returns (bool) {
        MultiProofGame game = MultiProofGame(gameAddress);
        if (!_matchesRoot(game, rootId, proofDomainHash, proofParentRef, proofL1OriginNumber, transition)) {
            return false;
        }

        if (!_matchesDomain(game, proofDomainHash, transition.rollupConfigHash, game.domain())) return false;

        return _matchesTransition(game, proofL1OriginNumber, transition);
    }

    function _matchesRoot(
        MultiProofGame game,
        bytes32 rootId,
        bytes32 proofDomainHash,
        address proofParentRef,
        uint256 proofL1OriginNumber,
        ProofLib.TransitionPublicValues memory transition
    ) private view returns (bool) {
        bytes32 reconstructedRootId = ProofLib.rootId(
            proofDomainHash,
            proofParentRef,
            transition.l2PostRoot,
            uint256(transition.l2PostBlockNumber),
            transition.l1Head,
            proofL1OriginNumber
        );

        return reconstructedRootId == rootId && game.rootId() == rootId && game.parentRef() == proofParentRef;
    }

    function _matchesDomain(
        MultiProofGame game,
        bytes32 proofDomainHash,
        bytes32 transitionRollupConfigHash,
        ProofLib.Domain memory gameDomain
    ) private view returns (bool) {
        return game.domainHash() == proofDomainHash && ProofLib.domainHash(gameDomain) == proofDomainHash
            && transitionRollupConfigHash == gameDomain.rollupConfigHash;
    }

    function _matchesTransition(
        MultiProofGame game,
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
