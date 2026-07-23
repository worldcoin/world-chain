// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofSystemGame} from "./interfaces/IWorldChainProofSystemGame.sol";
import {IWorldChainProofSystemFactory} from "./interfaces/IWorldChainProofSystemFactory.sol";
import {WorldChainProofLib} from "./WorldChainProofLib.sol";
import {Hash} from "./DisputeTypes.sol";

library WorldChainProofVerificationLib {
    /// @dev Validates the proof against the game's root, factory domain, and creation-time transition snapshot.
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

        WorldChainProofLib.Domain memory factoryDomain = _readFactoryDomain(game.factory());
        if (!_matchesDomain(game, proofDomainHash, transition.rollupConfigHash, factoryDomain)) return false;

        return _matchesTransition(game, proofL1OriginNumber, transition);
    }

    function _readFactoryDomain(address factory) private view returns (WorldChainProofLib.Domain memory domain) {
        (uint256 chainId, uint256 proofSystemVersion, bytes32 rollupConfigHash, uint256 blockInterval) =
            IWorldChainProofSystemFactory(factory).domain();
        domain = WorldChainProofLib.Domain({
            chainId: chainId,
            proofSystemVersion: proofSystemVersion,
            rollupConfigHash: rollupConfigHash,
            blockInterval: blockInterval
        });
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
        WorldChainProofLib.Domain memory factoryDomain
    ) private view returns (bool) {
        return game.domainHash() == proofDomainHash && WorldChainProofLib.domainHash(factoryDomain) == proofDomainHash
            && transitionRollupConfigHash == factoryDomain.rollupConfigHash;
    }

    function _matchesTransition(
        IWorldChainProofSystemGame game,
        uint256 proofL1OriginNumber,
        WorldChainProofLib.TransitionPublicValues memory transition
    ) private view returns (bool) {
        return game.startingRootClaim() == transition.l2PreRoot
            && game.startingL2BlockNumber() == uint256(transition.l2PreBlockNumber)
            && game.rootClaim() == transition.l2PostRoot
            && game.l2SequenceNumber() == uint256(transition.l2PostBlockNumber)
            && Hash.unwrap(game.l1Head()) == transition.l1Head && game.l1OriginNumber() == proofL1OriginNumber;
    }
}
