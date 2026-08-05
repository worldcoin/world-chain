// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IMultiProofGame} from "../interfaces/IMultiProofGame.sol";
import {ProofLib} from "./ProofLib.sol";
import {Hash} from "@optimism-bedrock/src/dispute/lib/Types.sol";

/// @title ProofVerificationLib
/// @author World Contributors
/// @custom:security-contact security@toolsforhumanity.com
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

        // The root id commits to (domainHash, parentRef, rootClaim, l2BlockNumber, l1Head,
        // l1OriginNumber). Matching both the id reconstructed from the proof's own values and
        // the game's self-reported id therefore proves, by collision resistance, that every
        // one of those fields equals the game's — no field-by-field comparison is needed.
        bytes32 reconstructedRootId = ProofLib.rootId(
            proofDomainHash,
            proofParentRef,
            transition.l2PostRoot,
            uint256(transition.l2PostBlockNumber),
            transition.l1Head,
            proofL1OriginNumber
        );
        if (reconstructedRootId != rootId || game.rootId() != rootId) return false;

        // Not covered by the root id: the rollup configuration (committed only inside the
        // opaque domain hash) and the pre-state snapshot (derived from the parent at creation).
        return game.rollupConfigHash() == transition.rollupConfigHash
            && Hash.unwrap(game.startingRootHash()) == transition.l2PreRoot
            && game.startingBlockNumber() == uint256(transition.l2PreBlockNumber);
    }
}
