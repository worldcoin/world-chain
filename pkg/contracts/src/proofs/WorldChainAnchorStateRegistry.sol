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
    error NonMonotonicRoot(uint256 currentL2BlockNumber, uint256 nextL2BlockNumber);
    error AnchorStateNotInAncestry(address game, bytes32 currentRootClaim, uint256 currentL2BlockNumber);

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
        blacklistedGames[game] = blacklisted;
        emit GameBlacklistedSet(game, blacklisted);
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

        if (proofGame.state() != WorldChainProofLib.RootState.FINALIZED) {
            revert GameNotFinalized(game);
        }

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
            IWorldChainProofSystemGame cursorGame = IWorldChainProofSystemGame(cursor);
            uint256 startingL2BlockNumber = cursorGame.startingL2BlockNumber();

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
