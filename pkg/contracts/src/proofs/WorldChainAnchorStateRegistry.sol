// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IDisputeGame} from "./external/IDisputeGame.sol";
import {GameStatus, GameType, Hash} from "./external/Types.sol";
import {IWorldChainProofSystemGame} from "./interfaces/IWorldChainProofSystemGame.sol";

/// @title WorldChainAnchorStateRegistry
/// @notice Tracks the canonical anchor for the World Chain proof game type and
///         the guardian-gated safety controls (pause, blacklist, retire). Only
///         finalized (`DEFENDER_WINS`), non-controlled roots may advance the
///         anchor, and only against a valid parent with a strictly increasing
///         L2 block number.
/// @dev Implements the subset of the OP `IAnchorStateRegistry` surface the proof
///      system needs. Anchor advancement is permissionless and self-validating;
///      the safety controls are guardian-only per WIP-1006.
contract WorldChainAnchorStateRegistry {
    error NotOwner();
    error RegistryPaused();
    error GameTypeRetired();
    error GameBlacklisted(address game);
    error GameNotFinalized(address game);
    error NonMonotonicRoot(uint256 currentL2BlockNumber, uint256 nextL2BlockNumber);
    error InvalidParent(address parentRef);
    error DuplicateBlockNumber(uint256 l2BlockNumber);

    event AnchorUpdated(address indexed game, bytes32 indexed rootId, bytes32 rootClaim, uint256 l2BlockNumber);
    event PausedSet(bool paused);
    event RetiredSet(bool retired);
    event GameBlacklistedSet(address indexed game, bool blacklisted);

    /// @notice Guardian / owner authorized for the safety controls.
    address public owner;

    /// @notice The respected game type for this registry.
    GameType public respectedGameTypeValue;

    /// @notice Whether anchor updates are paused.
    bool public paused;

    /// @notice Whether the game type has been retired.
    bool public retired;

    /// @notice Current anchor root commitment, claim, block number, and game.
    bytes32 public currentRootId;
    bytes32 public currentRootClaim;
    uint256 public currentL2BlockNumber;
    address public anchorGame;

    mapping(address game => bool accepted) public acceptedGames;
    mapping(bytes32 rootId => bool accepted) public acceptedRoots;
    mapping(address game => bool blacklisted) public blacklistedGames;
    mapping(uint256 l2BlockNumber => bool used) public usedBlockNumbers;

    /// @param startingRootClaim The genesis anchor claim.
    /// @param startingL2BlockNumber The genesis anchor L2 block number.
    /// @param respectedGameType_ The respected game type.
    constructor(bytes32 startingRootClaim, uint256 startingL2BlockNumber, GameType respectedGameType_) {
        owner = msg.sender;
        currentRootClaim = startingRootClaim;
        currentL2BlockNumber = startingL2BlockNumber;
        respectedGameTypeValue = respectedGameType_;
    }

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    /// @notice Transfers guardian ownership.
    function transferOwnership(address nextOwner) external onlyOwner {
        owner = nextOwner;
    }

    /// @notice Pauses or unpauses anchor updates (guardian-only).
    function setPaused(bool nextPaused) external onlyOwner {
        paused = nextPaused;
        emit PausedSet(nextPaused);
    }

    /// @notice Retires or un-retires the game type (guardian-only).
    function setRetired(bool nextRetired) external onlyOwner {
        retired = nextRetired;
        emit RetiredSet(nextRetired);
    }

    /// @notice Blacklists or un-blacklists a game (guardian-only).
    function setGameBlacklisted(address game, bool blacklisted) external onlyOwner {
        blacklistedGames[game] = blacklisted;
        emit GameBlacklistedSet(game, blacklisted);
    }

    /// @notice The respected game type.
    function respectedGameType() external view returns (GameType) {
        return respectedGameTypeValue;
    }

    /// @notice The current anchor root and its L2 block number.
    function getAnchorRoot() external view returns (Hash, uint256) {
        return (Hash.wrap(currentRootClaim), currentL2BlockNumber);
    }

    /// @notice Whether a finalized game's claim is acceptable to the registry.
    function isGameClaimValid(IDisputeGame game) public view returns (bool) {
        if (paused || retired) return false;
        if (blacklistedGames[address(game)]) return false;
        if (game.status() != GameStatus.DEFENDER_WINS) return false;

        IWorldChainProofSystemGame proofGame = IWorldChainProofSystemGame(address(game));
        address parent = proofGame.parentRef();
        if (parent != address(this) && !acceptedGames[parent]) return false;

        uint256 nextL2BlockNumber = game.l2BlockNumber();
        if (nextL2BlockNumber <= currentL2BlockNumber) return false;
        if (usedBlockNumbers[nextL2BlockNumber]) return false;
        return true;
    }

    /// @notice Whether a game is blacklisted.
    function isGameBlacklisted(IDisputeGame game) external view returns (bool) {
        return blacklistedGames[address(game)];
    }

    /// @notice Whether a game has resolved.
    function isGameResolved(IDisputeGame game) external view returns (bool) {
        return game.status() != GameStatus.IN_PROGRESS;
    }

    /// @notice Whether a game finalized in the defender's favor.
    function isGameFinalized(IDisputeGame game) external view returns (bool) {
        return game.status() == GameStatus.DEFENDER_WINS;
    }

    /// @notice Advances the anchor from a finalized, valid game. Permissionless.
    /// @dev Enforces parent validity, strict L2 block-number monotonicity, a
    ///      single accepted root per L2 block number, and that the game is not
    ///      blacklisted, retired, paused, or unfinalized.
    function setAnchorState(IDisputeGame game) external {
        if (paused) revert RegistryPaused();
        if (retired) revert GameTypeRetired();
        if (blacklistedGames[address(game)]) revert GameBlacklisted(address(game));

        if (game.status() != GameStatus.DEFENDER_WINS) {
            revert GameNotFinalized(address(game));
        }

        IWorldChainProofSystemGame proofGame = IWorldChainProofSystemGame(address(game));
        address parent = proofGame.parentRef();
        if (parent != address(this) && !acceptedGames[parent]) {
            revert InvalidParent(parent);
        }

        uint256 nextL2BlockNumber = game.l2BlockNumber();
        if (nextL2BlockNumber <= currentL2BlockNumber) {
            revert NonMonotonicRoot(currentL2BlockNumber, nextL2BlockNumber);
        }
        if (usedBlockNumbers[nextL2BlockNumber]) revert DuplicateBlockNumber(nextL2BlockNumber);

        bytes32 nextRootId = proofGame.rootId();
        currentRootId = nextRootId;
        currentRootClaim = bytes32(IDisputeGame(game).rootClaim().raw());
        currentL2BlockNumber = nextL2BlockNumber;
        anchorGame = address(game);
        acceptedGames[address(game)] = true;
        acceptedRoots[nextRootId] = true;
        usedBlockNumbers[nextL2BlockNumber] = true;

        emit AnchorUpdated(address(game), nextRootId, currentRootClaim, nextL2BlockNumber);
    }
}
