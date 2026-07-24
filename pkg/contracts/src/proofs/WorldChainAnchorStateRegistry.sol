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
    error FinalizedGameCannotBeBlacklisted(address game);
    error InvalidGameFactory(address expectedFactory, address actualFactory);
    error InvalidGameRegistry(address expectedRegistry, address actualRegistry);
    error UnregisteredGame(address game);
    error GameNotFinalized(address game);
    error NonMonotonicRoot(uint256 currentL2BlockNumber, uint256 nextL2BlockNumber);

    event AnchorUpdated(address indexed game, bytes32 indexed rootId, bytes32 rootClaim, uint256 l2BlockNumber);
    event FactoryInitialized(address indexed factory);
    event PausedSet(bool paused);
    event GameBlacklistedSet(address indexed game, bool blacklisted);

    address public owner;
    address public proofSystemFactory;
    bool public paused;

    bytes32 public currentRootId;
    bytes32 public currentRootClaim;
    uint256 public currentL2BlockNumber;
    address public anchorGame;

    mapping(address game => bool blacklisted) public blacklistedGames;

    constructor(bytes32 startingRootClaim, uint256 startingL2BlockNumber) {
        owner = msg.sender;
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
        paused = nextPaused;
        emit PausedSet(nextPaused);
    }

    function setGameBlacklisted(address game, bool blacklisted) external onlyOwner {
        // Descendant finality relies on finalized ancestor validity remaining immutable.
        if (blacklisted && isGameFinalized(game)) revert FinalizedGameCannotBeBlacklisted(game);

        blacklistedGames[game] = blacklisted;
        emit GameBlacklistedSet(game, blacklisted);
    }

    /// @notice Returns whether a game has finalized.
    function isGameFinalized(address game) public view returns (bool) {
        if (game.code.length == 0) return false;

        try IWorldChainProofSystemGame(game).state() returns (WorldChainProofLib.RootState gameState) {
            return gameState == WorldChainProofLib.RootState.FINALIZED;
        } catch {
            return false;
        }
    }

    /// @notice Returns whether a game is recognized by this registry and currently has a valid finalized claim.
    /// @dev Anchor advancement additionally requires a newer L2 block number.
    /// TODO: Before withdrawal integration, define OP-compatible retirement and emergency anchor recovery semantics,
    /// implement the IAnchorStateRegistry surface, and wire OptimismPortal to this predicate.
    function isGameClaimValid(address game) external view returns (bool) {
        address factory = proofSystemFactory;
        if (paused || factory == address(0) || blacklistedGames[game]) return false;
        if (!IWorldChainProofSystemFactory(factory).isFactoryGame(game)) return false;

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

        if (proofGame.state() != WorldChainProofLib.RootState.FINALIZED) revert GameNotFinalized(game);

        // A game can only finalize after its parent has finalized, so FINALIZED recursively certifies
        // its parent chain at resolution time without walking the whole chain again here.
        uint256 nextL2BlockNumber = proofGame.l2BlockNumber();
        if (nextL2BlockNumber <= currentL2BlockNumber) {
            revert NonMonotonicRoot(currentL2BlockNumber, nextL2BlockNumber);
        }

        bytes32 nextRootId = proofGame.rootId();
        currentRootId = nextRootId;
        currentRootClaim = proofGame.rootClaim();
        currentL2BlockNumber = nextL2BlockNumber;
        anchorGame = game;

        emit AnchorUpdated(game, nextRootId, currentRootClaim, nextL2BlockNumber);
    }
}
