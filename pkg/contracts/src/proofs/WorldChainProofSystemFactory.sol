// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {WorldChainProofLib} from "./WorldChainProofLib.sol";
import {Claim, GameId, GameType, Hash, Proposal, Timestamp, GameTypes} from "./DisputeTypes.sol";
import {WorldChainProofSystemGame} from "./WorldChainProofSystemGame.sol";
import {IWorldChainAnchorStateRegistry} from "./interfaces/IWorldChainAnchorStateRegistry.sol";
import {IDisputeGame} from "./interfaces/IDisputeGame.sol";
import {IOptimismPortal2DisputeGameFactory} from "./interfaces/IOptimismPortal2.sol";
import {IWorldChainProofSystemFactory} from "./interfaces/IWorldChainProofSystemFactory.sol";
import {IWorldChainProofVerifier} from "./interfaces/IWorldChainProofVerifier.sol";
import {IWorldChainProofSystemGame} from "./interfaces/IWorldChainProofSystemGame.sol";
import {IWorldChainStakingRegistry} from "./interfaces/IWorldChainStakingRegistry.sol";

contract WorldChainProofSystemFactory is IOptimismPortal2DisputeGameFactory {
    error GameAlreadyExists(bytes32 proposalKey, address game);
    error GameNotRetryable(bytes32 proposalKey, address game, WorldChainProofLib.InvalidationReason invalidationReason);
    error InvalidActivationParameters();
    error RegistryPaused();
    error RegistryFactoryMismatch(address expectedFactory, address actualFactory);
    error InvalidParent(address parentRef);
    error AnchorRegistryParentAlreadyUsed();
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

    mapping(bytes32 proposalKey => address game) private proposalGames;
    mapping(address game => bool created) private factoryGames;
    // Before the first anchor is established, only an invalidated starting game may be replaced.
    address private currentStartingGame;
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

    /// @notice Returns the indexed game selected by OptimismPortal2 when proving a withdrawal.
    function gameAtIndex(uint256 index) external view override returns (GameType, Timestamp, IDisputeGame) {
        address game = allGames[index];
        return
            (
                GameTypes.WIP_1006,
                Timestamp.wrap(WorldChainProofSystemGame(payable(game)).createdAt()),
                IDisputeGame(game)
            );
    }

    /// @notice Finds recent WIP-1006 games using the OP Stack ABI consumed by standard withdrawal tooling.
    /// @dev This preserves World Chain's custom proposal lifecycle while allowing callers to discover a game index.
    function findLatestGames(GameType gameType, uint256 start, uint256 n)
        external
        view
        returns (IWorldChainProofSystemFactory.GameSearchResult[] memory games_)
    {
        if (start >= allGames.length || n == 0 || GameType.unwrap(gameType) != GameType.unwrap(GameTypes.WIP_1006)) {
            return games_;
        }

        uint256 maxResults = n < start + 1 ? n : start + 1;
        games_ = new IWorldChainProofSystemFactory.GameSearchResult[](maxResults);

        for (uint256 resultIndex; resultIndex < maxResults; resultIndex++) {
            uint256 gameIndex = start - resultIndex;
            address gameAddress = allGames[gameIndex];
            IDisputeGame game = IDisputeGame(gameAddress);
            Timestamp timestamp = Timestamp.wrap(game.createdAt());

            games_[resultIndex] = IWorldChainProofSystemFactory.GameSearchResult({
                index: gameIndex,
                metadata: _packGameId(GameTypes.WIP_1006, timestamp, gameAddress),
                timestamp: timestamp,
                rootClaim: Claim.wrap(game.rootClaim()),
                extraData: game.extraData()
            });
        }
    }

    /// @notice Resolves game identity from its OP Stack game data for ASR registration checks.
    function games(GameType gameType, Claim rootClaim, bytes calldata extraData)
        external
        view
        returns (IDisputeGame, Timestamp)
    {
        if (GameType.unwrap(gameType) != GameType.unwrap(GameTypes.WIP_1006)) {
            return (IDisputeGame(address(0)), Timestamp.wrap(0));
        }

        (uint256 l2BlockNumber, address parentRef) = abi.decode(extraData, (uint256, address));
        address game = proposalGames[
            WorldChainProofLib.proposalKey(domainHash, parentRef, Claim.unwrap(rootClaim), l2BlockNumber)
        ];
        if (game == address(0)) return (IDisputeGame(address(0)), Timestamp.wrap(0));

        return (IDisputeGame(game), Timestamp.wrap(WorldChainProofSystemGame(payable(game)).createdAt()));
    }

    function propose(address parentRef, bytes32 rootClaim, uint256 l2BlockNumber)
        external
        payable
        returns (address game, bytes32 id)
    {
        uint256 l1OriginNumber = block.number == 0 ? 0 : block.number - 1;
        bytes32 l1OriginHash = block.number == 0 ? bytes32(0) : blockhash(l1OriginNumber);

        bytes32 key = WorldChainProofLib.proposalKey(domainHash, parentRef, rootClaim, l2BlockNumber);
        address existing = proposalGames[key];
        if (parentRef == address(anchorStateRegistry)) {
            if (anchorStateRegistry.anchorGame() != address(0)) revert AnchorRegistryParentAlreadyUsed();

            address startingGame = currentStartingGame;
            if (
                startingGame != address(0) && startingGame != existing
                    && IWorldChainProofSystemGame(startingGame).state() != WorldChainProofLib.RootState.INVALIDATED
            ) {
                revert AnchorRegistryParentAlreadyUsed();
            }
        }

        uint256 attempt;
        if (existing != address(0)) {
            IWorldChainProofSystemGame existingGame = IWorldChainProofSystemGame(existing);
            if (existingGame.state() != WorldChainProofLib.RootState.INVALIDATED) {
                revert GameAlreadyExists(key, existing);
            }

            WorldChainProofLib.InvalidationReason reason = existingGame.invalidationReason();
            // Only direct timeouts retry the same transition. Inherited invalidations must rebase on the replacement parent.
            if (reason != WorldChainProofLib.InvalidationReason.PROOF_TIMEOUT) {
                revert GameNotRetryable(key, existing, reason);
            }
            attempt = existingGame.attempt() + 1;
        }

        (bytes32 startingRootClaim, uint256 startingL2BlockNumber) = _validateParent(parentRef, l2BlockNumber);

        id = WorldChainProofLib.rootId(domainHash, parentRef, rootClaim, l2BlockNumber, l1OriginHash, l1OriginNumber);

        game = address(
            new WorldChainProofSystemGame{value: msg.value}(
                WorldChainProofSystemGame.ProposalInit({
                    factory: address(this),
                    anchorStateRegistry: address(anchorStateRegistry),
                    attempt: attempt,
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
        proposalGames[key] = game;
        factoryGames[game] = true;
        if (parentRef == address(anchorStateRegistry)) currentStartingGame = game;
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

    function _packGameId(GameType gameType, Timestamp timestamp, address game) private pure returns (GameId) {
        // OP packs the 32-bit game type, 64-bit creation timestamp, and 160-bit game address into one word.
        return GameId.wrap(
            bytes32(
                (uint256(GameType.unwrap(gameType)) << 224) | (uint256(Timestamp.unwrap(timestamp)) << 160)
                    | uint256(uint160(game))
            )
        );
    }

    function _validateParent(address parentRef, uint256 l2BlockNumber)
        private
        view
        returns (bytes32 startingRootClaim, uint256 startingL2BlockNumber)
    {
        if (anchorStateRegistry.paused()) revert RegistryPaused();

        address configuredFactory = anchorStateRegistry.disputeGameFactory();
        if (configuredFactory != address(this)) {
            revert RegistryFactoryMismatch(address(this), configuredFactory);
        }

        // Until the first game is anchored, the registry sentinel is the only identity for the deployment anchor.
        if (parentRef == address(anchorStateRegistry)) {
            Proposal memory startingAnchor = anchorStateRegistry.getStartingAnchorRoot();
            startingRootClaim = Hash.unwrap(startingAnchor.root);
            startingL2BlockNumber = startingAnchor.l2SequenceNumber;
        } else {
            if (!factoryGames[parentRef]) revert InvalidParent(parentRef);
            if (anchorStateRegistry.blacklistedGames(parentRef)) revert ParentGameBlacklisted(parentRef);

            IWorldChainProofSystemGame parentGame = IWorldChainProofSystemGame(parentRef);
            if (
                parentGame.factory() != address(this)
                    || parentGame.anchorStateRegistry() != address(anchorStateRegistry)
                    || parentGame.domainHash() != domainHash
            ) {
                revert InvalidParent(parentRef);
            }

            WorldChainProofLib.RootState parentState = parentGame.state();
            if (parentState == WorldChainProofLib.RootState.INVALIDATED) {
                revert InvalidParentState(parentRef, WorldChainProofLib.RootState.INVALIDATED);
            }

            startingRootClaim = parentGame.rootClaim();
            startingL2BlockNumber = parentGame.l2SequenceNumber();
        }

        // TODO(PROTO-4907): Confirm whether proposals require an exact block interval or only a bounded range.
        uint256 expectedL2BlockNumber = startingL2BlockNumber + domain.blockInterval;
        if (l2BlockNumber != expectedL2BlockNumber) {
            revert InvalidL2BlockNumber(expectedL2BlockNumber, l2BlockNumber);
        }

        (, uint256 currentAnchorBlock) = anchorStateRegistry.getAnchorRoot();
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
