// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IMultiProofGame} from "../interfaces/IMultiProofGame.sol";
import {ProofLib} from "./ProofLib.sol";
import {Claim, Hash} from "@optimism-bedrock/src/dispute/lib/Types.sol";

library ProofVerificationLib {
    /// @dev Validates the proof against the game's root, domain, and creation-time transition snapshot.
    function matchesGame(
        address gameAddress,
        bytes32 rootId,
        bytes32 proofDomainHash,
        address proofParentRef,
        uint256 proofL1OriginNumber,
        ProofLib.TransitionPublicValues memory transition
    ) internal view returns (bool) {
        IMultiProofGame game = IMultiProofGame(gameAddress);

        // The root id binds every transition field; recompute it from the proof's own values.
        bytes32 reconstructedRootId = ProofLib.rootId(
            proofDomainHash,
            proofParentRef,
            transition.l2PostRoot,
            uint256(transition.l2PostBlockNumber),
            transition.l1Head,
            proofL1OriginNumber
        );
        if (reconstructedRootId != rootId || game.rootId() != rootId) return false;

        // The proof must target this deployment's domain and rollup configuration.
        if (game.domainHash() != proofDomainHash || game.rollupConfigHash() != transition.rollupConfigHash) {
            return false;
        }

        // The transition must match the game's creation-time snapshot.
        return game.parentRef() == proofParentRef && Hash.unwrap(game.startingRootHash()) == transition.l2PreRoot
            && game.startingBlockNumber() == uint256(transition.l2PreBlockNumber)
            && Claim.unwrap(game.rootClaim()) == transition.l2PostRoot
            && game.l2SequenceNumber() == uint256(transition.l2PostBlockNumber)
            && Hash.unwrap(game.l1Head()) == transition.l1Head && game.l1OriginNumber() == proofL1OriginNumber;
    }
}
