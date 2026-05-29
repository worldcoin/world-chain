// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofSystemGame} from "./interfaces/IWorldChainProofSystemGame.sol";

contract WorldChainAnchorStateRegistry {
    error NotOwner();
    error RegistryPaused();
    error GameBlacklisted(address game);
    error GameNotFinalized(address game);
    error NonMonotonicRoot(uint256 currentL2BlockNumber, uint256 nextL2BlockNumber);
    error InvalidParent(address parentRef);

    event AnchorUpdated(address indexed game, bytes32 indexed rootId, bytes32 rootClaim, uint256 l2BlockNumber);
    event PausedSet(bool paused);
    event GameBlacklistedSet(address indexed game, bool blacklisted);

    address public owner;
    bool public paused;

    bytes32 public currentRootId;
    bytes32 public currentRootClaim;
    uint256 public currentL2BlockNumber;
    address public anchorGame;

    mapping(address game => bool accepted) public acceptedGames;
    mapping(bytes32 rootId => bool accepted) public acceptedRoots;
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

        IWorldChainProofSystemGame proofGame = IWorldChainProofSystemGame(game);
        if (proofGame.state() != IWorldChainProofSystemGame.RootState.FINALIZED) {
            revert GameNotFinalized(game);
        }

        address parent = proofGame.parentRef();
        if (parent != address(this) && !acceptedGames[parent]) {
            revert InvalidParent(parent);
        }

        uint256 nextL2BlockNumber = proofGame.l2BlockNumber();
        if (nextL2BlockNumber <= currentL2BlockNumber) {
            revert NonMonotonicRoot(currentL2BlockNumber, nextL2BlockNumber);
        }

        bytes32 nextRootId = proofGame.rootId();
        currentRootId = nextRootId;
        currentRootClaim = proofGame.rootClaim();
        currentL2BlockNumber = nextL2BlockNumber;
        anchorGame = game;
        acceptedGames[game] = true;
        acceptedRoots[nextRootId] = true;

        emit AnchorUpdated(game, nextRootId, currentRootClaim, nextL2BlockNumber);
    }
}
