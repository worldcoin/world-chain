// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofSystemGame} from "./interfaces/IWorldChainProofSystemGame.sol";
import {WorldChainProofLib} from "./WorldChainProofLib.sol";

library WorldChainProofVerificationLib {
    /// @dev Existing games remain bound to their creation-time snapshot when their parent or anchor later advances.
    function matchesGame(
        address gameAddress,
        address expectedAnchorStateRegistry,
        bytes32 rootId,
        bytes32 domainHash,
        address parentRef,
        uint256 l1OriginNumber,
        WorldChainProofLib.TransitionPublicValues memory transition
    ) internal view returns (bool) {
        IWorldChainProofSystemGame game = IWorldChainProofSystemGame(gameAddress);
        return game.rootId() == rootId && game.anchorStateRegistry() == expectedAnchorStateRegistry
            && game.domainHash() == domainHash && game.parentRef() == parentRef
            && game.startingRootClaim() == transition.l2PreRoot
            && game.startingL2BlockNumber() == uint256(transition.l2PreBlockNumber)
            && game.rootClaim() == transition.l2PostRoot
            && game.l2BlockNumber() == uint256(transition.l2PostBlockNumber) && game.l1OriginHash() == transition.l1Head
            && game.l1OriginNumber() == l1OriginNumber;
    }
}
