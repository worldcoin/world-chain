// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofSystemGame} from "./interfaces/IWorldChainProofSystemGame.sol";
import {IWorldChainProofSystemFactory} from "./interfaces/IWorldChainProofSystemFactory.sol";
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
        if (game.rootId() != rootId || game.domainHash() != domainHash || game.parentRef() != parentRef) return false;

        (uint256 chainId, uint256 proofSystemVersion, bytes32 rollupConfigHash, uint256 blockInterval) =
            IWorldChainProofSystemFactory(game.factory()).domain();
        WorldChainProofLib.Domain memory expectedDomain = WorldChainProofLib.Domain({
            chainId: chainId,
            proofSystemVersion: proofSystemVersion,
            rollupConfigHash: rollupConfigHash,
            blockInterval: blockInterval
        });
        if (WorldChainProofLib.domainHash(expectedDomain) != domainHash) return false;

        return transition.rollupConfigHash == rollupConfigHash && game.startingRootClaim() == transition.l2PreRoot
            && game.startingL2BlockNumber() == uint256(transition.l2PreBlockNumber)
            && game.rootClaim() == transition.l2PostRoot
            && game.l2BlockNumber() == uint256(transition.l2PostBlockNumber) && game.l1OriginHash() == transition.l1Head
            && game.l1OriginNumber() == l1OriginNumber;
    }
}
