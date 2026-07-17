// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {WorldChainProofLib} from "./WorldChainProofLib.sol";
import {WorldChainProofSystemGame} from "./WorldChainProofSystemGame.sol";
import {IWorldChainAnchorStateRegistry} from "./interfaces/IWorldChainAnchorStateRegistry.sol";
import {IWorldChainProofVerifier} from "./interfaces/IWorldChainProofVerifier.sol";
import {IWorldChainProofSystemGame} from "./interfaces/IWorldChainProofSystemGame.sol";
import {IWorldChainStakingRegistry} from "./interfaces/IWorldChainStakingRegistry.sol";

contract WorldChainProofSystemFactory {
    error GameAlreadyExists(bytes32 proposalKey, address game);
    error InvalidActivationParameters();
    error RegistryPaused();
    error RegistryFactoryMismatch(address expectedFactory, address actualFactory);
    error InvalidParent(address parentRef);
    error InvalidParentState(address parentRef, WorldChainProofLib.RootState state);
    error ParentGameBlacklisted(address parentRef);
    error InvalidL2BlockNumber(uint256 expectedL2BlockNumber, uint256 actualL2BlockNumber);
    error TargetBlockNotNewer(uint256 currentL2BlockNumber, uint256 targetL2BlockNumber);

    event GameCreated(
        bytes32 indexed proposalKey,
        bytes32 indexed rootId,
        address indexed game,
        address proposer,
        bytes32 rootClaim,
        uint256 l2BlockNumber,
        address parentRef,
        bytes32 l1OriginHash,
        uint256 l1OriginNumber
    );

    struct GameCreatedLog {
        bytes32 proposalKey;
        bytes32 rootId;
        address game;
        address proposer;
        bytes32 rootClaim;
        uint256 l2BlockNumber;
        address parentRef;
        bytes32 l1OriginHash;
        uint256 l1OriginNumber;
    }

    WorldChainProofLib.Domain public domain;
    bytes32 public immutable domainHash;
    IWorldChainAnchorStateRegistry public immutable anchorStateRegistry;

    uint64 public immutable challengePeriod;
    uint64 public immutable proofPeriod;
    uint256 public immutable proposerBond;
    uint256 public immutable challengerBond;
    uint8 public immutable proofThreshold;

    IWorldChainProofVerifier public immutable validityProofVerifier;
    IWorldChainProofVerifier public immutable teeVerifier;
    IWorldChainProofVerifier public immutable securityCouncil;
    IWorldChainStakingRegistry public immutable stakingRegistry;

    mapping(bytes32 proposalKey => address game) public games;
    mapping(address game => bool created) public isFactoryGame;
    // Lets stateless services reconstruct all games after restart without relying on historical logs.
    address[] private allGames;

    constructor(
        WorldChainProofLib.Domain memory domain_,
        IWorldChainAnchorStateRegistry anchorStateRegistry_,
        uint64 challengePeriod_,
        uint64 proofPeriod_,
        uint256 proposerBond_,
        uint256 challengerBond_,
        uint8 proofThreshold_,
        IWorldChainProofVerifier validityProofVerifier_,
        IWorldChainProofVerifier teeVerifier_,
        IWorldChainProofVerifier securityCouncil_,
        IWorldChainStakingRegistry stakingRegistry_
    ) {
        if (
            challengePeriod_ == 0 || proofPeriod_ == 0 || domain_.chainId == 0 || domain_.proofSystemVersion == 0
                || domain_.blockInterval == 0 || address(anchorStateRegistry_) == address(0) || proofThreshold_ == 0
                || proofThreshold_ > WorldChainProofLib.PROOF_LANE_COUNT
        ) {
            revert InvalidActivationParameters();
        }

        domain = domain_;
        domainHash = WorldChainProofLib.domainHash(domain_);
        anchorStateRegistry = anchorStateRegistry_;
        challengePeriod = challengePeriod_;
        proofPeriod = proofPeriod_;
        proposerBond = proposerBond_;
        challengerBond = challengerBond_;
        proofThreshold = proofThreshold_;
        validityProofVerifier = validityProofVerifier_;
        teeVerifier = teeVerifier_;
        securityCouncil = securityCouncil_;
        stakingRegistry = stakingRegistry_;
    }

    function gameCount() external view returns (uint256) {
        return allGames.length;
    }

    function gameAt(uint256 index) external view returns (address) {
        return allGames[index];
    }

    function propose(address parentRef, bytes32 rootClaim, uint256 l2BlockNumber)
        external
        payable
        returns (address game, bytes32 id)
    {
        uint256 l1OriginNumber = block.number == 0 ? 0 : block.number - 1;
        bytes32 l1OriginHash = block.number == 0 ? bytes32(0) : blockhash(l1OriginNumber);

        bytes32 key = WorldChainProofLib.proposalKey(domainHash, parentRef, rootClaim, l2BlockNumber);
        address existing = games[key];
        if (existing != address(0)) revert GameAlreadyExists(key, existing);

        (bytes32 startingRootClaim, uint256 startingL2BlockNumber) = _validateParent(parentRef, l2BlockNumber);

        id = WorldChainProofLib.rootId(domainHash, parentRef, rootClaim, l2BlockNumber, l1OriginHash, l1OriginNumber);

        game = address(
            new WorldChainProofSystemGame{value: msg.value}(
                WorldChainProofSystemGame.ProposalInit({
                    factory: address(this),
                    anchorStateRegistry: address(anchorStateRegistry),
                    attempt: 0,
                    proposer: msg.sender,
                    parentRef: parentRef,
                    startingRootClaim: startingRootClaim,
                    startingL2BlockNumber: startingL2BlockNumber,
                    rootClaim: rootClaim,
                    l2BlockNumber: l2BlockNumber,
                    l1OriginHash: l1OriginHash,
                    l1OriginNumber: l1OriginNumber
                }),
                WorldChainProofSystemGame.ActivationConfig({
                    domainHash: domainHash,
                    challengePeriod: challengePeriod,
                    proofPeriod: proofPeriod,
                    proposerBond: proposerBond,
                    challengerBond: challengerBond,
                    proofThreshold: proofThreshold,
                    validityProofVerifier: validityProofVerifier,
                    teeVerifier: teeVerifier,
                    securityCouncil: securityCouncil,
                    stakingRegistry: stakingRegistry
                })
            )
        );
        games[key] = game;
        isFactoryGame[game] = true;
        allGames.push(game);

        _emitGameCreated(
            GameCreatedLog({
                proposalKey: key,
                rootId: id,
                game: game,
                proposer: msg.sender,
                rootClaim: rootClaim,
                l2BlockNumber: l2BlockNumber,
                parentRef: parentRef,
                l1OriginHash: l1OriginHash,
                l1OriginNumber: l1OriginNumber
            })
        );
    }

    function computeProposalKey(address parentRef, bytes32 rootClaim, uint256 l2BlockNumber)
        external
        view
        returns (bytes32)
    {
        return WorldChainProofLib.proposalKey(domainHash, parentRef, rootClaim, l2BlockNumber);
    }

    function computeRootId(
        address parentRef,
        bytes32 rootClaim,
        uint256 l2BlockNumber,
        bytes32 l1OriginHash,
        uint256 l1OriginNumber
    ) external view returns (bytes32) {
        return WorldChainProofLib.rootId(domainHash, parentRef, rootClaim, l2BlockNumber, l1OriginHash, l1OriginNumber);
    }

    function _validateParent(address parentRef, uint256 l2BlockNumber)
        private
        view
        returns (bytes32 startingRootClaim, uint256 startingL2BlockNumber)
    {
        if (anchorStateRegistry.paused()) revert RegistryPaused();

        address configuredFactory = anchorStateRegistry.proofSystemFactory();
        if (configuredFactory != address(this)) {
            revert RegistryFactoryMismatch(address(this), configuredFactory);
        }

        // The registry is the parent sentinel for transitions starting from the current accepted anchor.
        if (parentRef == address(anchorStateRegistry)) {
            startingRootClaim = anchorStateRegistry.currentRootClaim();
            startingL2BlockNumber = anchorStateRegistry.currentL2BlockNumber();
        } else {
            if (!isFactoryGame[parentRef]) revert InvalidParent(parentRef);
            if (anchorStateRegistry.blacklistedGames(parentRef)) revert ParentGameBlacklisted(parentRef);

            IWorldChainProofSystemGame parentGame = IWorldChainProofSystemGame(parentRef);
            if (
                parentGame.factory() != address(this)
                    || parentGame.anchorStateRegistry() != address(anchorStateRegistry)
                    || parentGame.domainHash() != domainHash
            ) {
                revert InvalidParent(parentRef);
            }

            IWorldChainProofSystemGame.RootState parentState = parentGame.state();
            if (parentState == IWorldChainProofSystemGame.RootState.INVALIDATED) {
                revert InvalidParentState(parentRef, WorldChainProofLib.RootState.INVALIDATED);
            }

            startingRootClaim = parentGame.rootClaim();
            startingL2BlockNumber = parentGame.l2BlockNumber();
        }

        // TODO(PROTO-4907): Confirm whether proposals require an exact block interval or only a bounded range.
        uint256 expectedL2BlockNumber = startingL2BlockNumber + domain.blockInterval;
        if (l2BlockNumber != expectedL2BlockNumber) {
            revert InvalidL2BlockNumber(expectedL2BlockNumber, l2BlockNumber);
        }

        uint256 currentAnchorBlock = anchorStateRegistry.currentL2BlockNumber();
        if (l2BlockNumber <= currentAnchorBlock) {
            revert TargetBlockNotNewer(currentAnchorBlock, l2BlockNumber);
        }
    }

    function _emitGameCreated(GameCreatedLog memory log) private {
        emit GameCreated(
            log.proposalKey,
            log.rootId,
            log.game,
            log.proposer,
            log.rootClaim,
            log.l2BlockNumber,
            log.parentRef,
            log.l1OriginHash,
            log.l1OriginNumber
        );
    }
}
