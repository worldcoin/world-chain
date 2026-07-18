// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {WorldChainProofLib} from "./WorldChainProofLib.sol";
import {IWorldChainProofSystemGame} from "./interfaces/IWorldChainProofSystemGame.sol";
import {IWorldChainProofSystemFactory} from "./interfaces/IWorldChainProofSystemFactory.sol";

contract WorldChainAnchorStateRegistry {
    error NotOwner();
    error InvalidFactory(address factory);
    error FactoryAlreadyInitialized(address factory);
    error FactoryNotInitialized();
    error RegistryPaused();
    error GameBlacklisted(address game);
    error InvalidGameFactory(address expectedFactory, address actualFactory);
    error InvalidGameRegistry(address expectedRegistry, address actualRegistry);
    error UnregisteredGame(address game);
    error GameNotFinalized(address game);
    error GameNotMature(address game, uint256 eligibleAt);
    error GameRetired(address game);
    error CurrentAnchorInvalid(address game);
    error NonMonotonicRoot(uint256 currentL2BlockNumber, uint256 nextL2BlockNumber);
    error AnchorStateNotInAncestry(address game, bytes32 currentRootClaim, uint256 currentL2BlockNumber);

    event AnchorUpdated(address indexed game, bytes32 indexed rootId, bytes32 rootClaim, uint256 l2BlockNumber);
    event FactoryInitialized(address indexed factory);
    event PausedSet(bool paused);
    event GameBlacklistedSet(address indexed game, bool blacklisted);
    event RetirementTimestampUpdated(uint64 timestamp);

    address public owner;
    address public proofSystemFactory;
    uint64 public immutable finalityDelay;
    uint64 public retirementTimestamp;
    bool public paused;

    bytes32 public currentRootId;
    bytes32 public currentRootClaim;
    uint256 public currentL2BlockNumber;
    address public anchorGame;

    mapping(address game => bool blacklisted) public blacklistedGames;

    constructor(bytes32 startingRootClaim, uint256 startingL2BlockNumber, uint64 finalityDelay_) {
        owner = msg.sender;
        finalityDelay = finalityDelay_;
        currentRootClaim = startingRootClaim;
        currentL2BlockNumber = startingL2BlockNumber;
    }

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    function transferOwnership(address nextOwner) external onlyOwner {
        owner = nextOwner;
    }

    function initializeFactory(address factory) external onlyOwner {
        if (factory == address(0)) revert InvalidFactory(factory);
        if (proofSystemFactory != address(0)) revert FactoryAlreadyInitialized(proofSystemFactory);

        proofSystemFactory = factory;
        emit FactoryInitialized(factory);
    }

    function setPaused(bool nextPaused) external onlyOwner {
        if (!nextPaused) {
            address currentAnchor = anchorGame;
            if (currentAnchor != address(0) && (blacklistedGames[currentAnchor] || isGameRetired(currentAnchor))) {
                revert CurrentAnchorInvalid(currentAnchor);
            }
        }

        paused = nextPaused;
        emit PausedSet(nextPaused);
    }

    /// @notice Blacklists one game. Use `updateRetirementTimestamp` to invalidate all existing games.
    function setGameBlacklisted(address game, bool blacklisted) external onlyOwner {
        blacklistedGames[game] = blacklisted;
        emit GameBlacklistedSet(game, blacklisted);

        // Advancing from a newly distrusted accepted checkpoint must stop atomically.
        if (blacklisted && game == anchorGame && !paused) {
            paused = true;
            emit PausedSet(true);
        }
    }

    /// @notice Invalidates every game created up to the current timestamp and pauses anchor progression.
    function updateRetirementTimestamp() external onlyOwner {
        retirementTimestamp = uint64(block.timestamp);
        emit RetirementTimestampUpdated(retirementTimestamp);

        if (!paused) {
            paused = true;
            emit PausedSet(true);
        }
    }

    /// @notice Returns whether a game has finalized and passed the registry's post-resolution delay.
    function isGameFinalized(address game) public view returns (bool) {
        if (game.code.length == 0) return false;

        IWorldChainProofSystemGame proofGame = IWorldChainProofSystemGame(game);
        try proofGame.state() returns (WorldChainProofLib.RootState gameState) {
            if (gameState != WorldChainProofLib.RootState.FINALIZED) return false;
        } catch {
            return false;
        }

        try proofGame.finalizedAt() returns (uint64 finalizedAt) {
            return block.timestamp >= uint256(finalizedAt) + finalityDelay;
        } catch {
            return false;
        }
    }

    /// @notice Returns whether a game was created before the latest blanket invalidation.
    function isGameRetired(address game) public view returns (bool) {
        uint64 retiredAt = retirementTimestamp;
        if (retiredAt == 0 || game.code.length == 0) return false;

        try IWorldChainProofSystemGame(game).createdAt() returns (uint64 createdAt) {
            return createdAt <= retiredAt;
        } catch {
            return false;
        }
    }

    /// @notice Returns whether a game is recognized by this registry and currently has a valid finalized claim.
    /// @dev Anchor advancement additionally requires a newer block and continuity with the current accepted anchor.
    /// TODO: Before withdrawal integration, implement the OP IAnchorStateRegistry surface and wire OptimismPortal
    /// to this predicate after confirming the emergency anchor recovery semantics.
    function isGameClaimValid(address game) external view returns (bool) {
        address factory = proofSystemFactory;
        if (paused || factory == address(0) || blacklistedGames[game]) return false;
        if (!IWorldChainProofSystemFactory(factory).isFactoryGame(game)) return false;
        if (isGameRetired(game)) return false;

        IWorldChainProofSystemGame proofGame = IWorldChainProofSystemGame(game);
        return
            proofGame.factory() == factory && proofGame.anchorStateRegistry() == address(this) && isGameFinalized(game);
    }

    function setAnchorState(address game) external {
        if (paused) revert RegistryPaused();
        if (blacklistedGames[game]) revert GameBlacklisted(game);

        address factory = proofSystemFactory;
        if (factory == address(0)) revert FactoryNotInitialized();

        IWorldChainProofSystemGame proofGame = IWorldChainProofSystemGame(game);
        if (proofGame.factory() != factory) revert InvalidGameFactory(factory, proofGame.factory());
        if (proofGame.anchorStateRegistry() != address(this)) {
            revert InvalidGameRegistry(address(this), proofGame.anchorStateRegistry());
        }
        if (!IWorldChainProofSystemFactory(factory).isFactoryGame(game)) revert UnregisteredGame(game);
        if (isGameRetired(game)) revert GameRetired(game);

        if (proofGame.state() != WorldChainProofLib.RootState.FINALIZED) revert GameNotFinalized(game);

        uint256 eligibleAt = uint256(proofGame.finalizedAt()) + finalityDelay;
        if (block.timestamp < eligibleAt) revert GameNotMature(game, eligibleAt);

        // Parent-aware resolution guarantees that every ancestor of a finalized game is finalized,
        // so intermediate games do not need separate anchor transactions.
        uint256 nextL2BlockNumber = proofGame.l2BlockNumber();
        if (nextL2BlockNumber <= currentL2BlockNumber) {
            revert NonMonotonicRoot(currentL2BlockNumber, nextL2BlockNumber);
        }
        _requireExtendsCurrentAnchor(game, factory);

        bytes32 nextRootId = proofGame.rootId();
        currentRootId = nextRootId;
        currentRootClaim = proofGame.rootClaim();
        currentL2BlockNumber = nextL2BlockNumber;
        anchorGame = game;

        emit AnchorUpdated(game, nextRootId, currentRootClaim, nextL2BlockNumber);
    }

    function _requireExtendsCurrentAnchor(address candidate, address factory) private view {
        bytes32 anchorRootClaim = currentRootClaim;
        uint256 anchorL2BlockNumber = currentL2BlockNumber;
        address cursor = candidate;

        // Finalization proves that the candidate's own ancestry settled, but not that it extends
        // the registry's currently accepted checkpoint. Transition snapshots preserve that link
        // even when the registry sentinel, rather than anchorGame, is used as the direct parent.
        // Traversal is linear in skipped games; callers can advance through finalized checkpoints
        // in smaller chunks when a single jump would exceed the transaction gas limit.
        while (true) {
            if (blacklistedGames[cursor]) revert GameBlacklisted(cursor);

            IWorldChainProofSystemGame cursorGame = IWorldChainProofSystemGame(cursor);
            uint256 startingL2BlockNumber = cursorGame.startingL2BlockNumber();

            // A transition starting from the current root and block proves that the candidate
            // extends the accepted checkpoint, even if it uses a different game lineage.
            if (startingL2BlockNumber == anchorL2BlockNumber) {
                if (cursorGame.startingRootClaim() != anchorRootClaim) {
                    revert AnchorStateNotInAncestry(candidate, anchorRootClaim, anchorL2BlockNumber);
                }
                return;
            }
            if (startingL2BlockNumber < anchorL2BlockNumber) {
                revert AnchorStateNotInAncestry(candidate, anchorRootClaim, anchorL2BlockNumber);
            }

            address parent = cursorGame.parentRef();
            if (parent == address(this) || !IWorldChainProofSystemFactory(factory).isFactoryGame(parent)) {
                revert AnchorStateNotInAncestry(candidate, anchorRootClaim, anchorL2BlockNumber);
            }
            cursor = parent;
        }
    }
}
