// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofSystemGame} from "./interfaces/IWorldChainProofSystemGame.sol";
import {WorldChainProofLib} from "./WorldChainProofLib.sol";

library WorldChainProofVerificationLib {
    /// @dev Validates the proof against the game's root, domain, and creation-time transition snapshot.
    function matchesGame(
        address gameAddress,
        bytes32 rootId,
        bytes32 proofDomainHash,
        address proofParentRef,
        uint256 proofL1OriginNumber,
        WorldChainProofLib.TransitionPublicValues memory transition
    ) internal view returns (bool) {
        IWorldChainProofSystemGame game = IWorldChainProofSystemGame(gameAddress);
        if (!_matchesRoot(game, rootId, proofDomainHash, proofParentRef, proofL1OriginNumber, transition)) {
            return false;
        }

        if (!_matchesDomain(game, proofDomainHash, transition.rollupConfigHash, game.domain())) return false;

        return _matchesTransition(game, proofL1OriginNumber, transition);
    }

    function _matchesRoot(
        IWorldChainProofSystemGame game,
        bytes32 rootId,
        bytes32 proofDomainHash,
        address proofParentRef,
        uint256 proofL1OriginNumber,
        WorldChainProofLib.TransitionPublicValues memory transition
    ) private view returns (bool) {
        bytes32 reconstructedRootId =
            WorldChainProofLib.rootId(
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
        IWorldChainProofSystemGame game,
        bytes32 proofDomainHash,
        bytes32 transitionRollupConfigHash,
        WorldChainProofLib.Domain memory gameDomain
    ) private view returns (bool) {
        return game.domainHash() == proofDomainHash && WorldChainProofLib.domainHash(gameDomain) == proofDomainHash
            && transitionRollupConfigHash == gameDomain.rollupConfigHash;
    }

    function _matchesTransition(
        IWorldChainProofSystemGame game,
        uint256 proofL1OriginNumber,
        WorldChainProofLib.TransitionPublicValues memory transition
    ) private view returns (bool) {
        return game.startingRootClaim() == transition.l2PreRoot
            && game.startingL2BlockNumber() == uint256(transition.l2PreBlockNumber)
            && game.rootClaim() == transition.l2PostRoot
            && game.l2BlockNumber() == uint256(transition.l2PostBlockNumber) && game.l1OriginHash() == transition.l1Head
            && game.l1OriginNumber() == proofL1OriginNumber;
    }
}
