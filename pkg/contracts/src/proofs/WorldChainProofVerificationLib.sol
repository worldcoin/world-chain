// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofSystemGame} from "./interfaces/IWorldChainProofSystemGame.sol";
import {WorldChainProofLib} from "./WorldChainProofLib.sol";

library WorldChainProofVerificationLib {
    /// @dev Validates the proof's identity commitment before comparing it with the game's creation-time snapshot.
    function matchesGame(
        address gameAddress,
        bytes32 rootId,
        bytes32 domainHash,
        address parentRef,
        uint256 l1OriginNumber,
        WorldChainProofLib.TransitionPublicValues memory transition
    ) internal view returns (bool) {
        bytes32 expectedRootId = WorldChainProofLib.rootId(
            domainHash,
            parentRef,
            transition.l2PostRoot,
            uint256(transition.l2PostBlockNumber),
            transition.l1Head,
            l1OriginNumber
        );
        if (expectedRootId != rootId) return false;

        IWorldChainProofSystemGame game = IWorldChainProofSystemGame(gameAddress);
        return game.rootId() == rootId && game.domainHash() == domainHash && game.parentRef() == parentRef
            && game.startingRootClaim() == transition.l2PreRoot
            && game.startingL2BlockNumber() == uint256(transition.l2PreBlockNumber)
            && game.rootClaim() == transition.l2PostRoot
            && game.l2BlockNumber() == uint256(transition.l2PostBlockNumber) && game.l1OriginHash() == transition.l1Head
            && game.l1OriginNumber() == l1OriginNumber;
    }
}
