// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Claim, GameStatus, GameType, Hash, Proposal, Timestamp, GameTypes} from "./DisputeTypes.sol";
import {IDisputeGame} from "./interfaces/IDisputeGame.sol";
import {IWorldChainProofSystemGame} from "./interfaces/IWorldChainProofSystemGame.sol";
import {IWorldChainProofSystemFactory} from "./interfaces/IWorldChainProofSystemFactory.sol";
import {IOptimismPortal2AnchorStateRegistry} from "./interfaces/IOptimismPortal2.sol";

contract WorldChainAnchorStateRegistry is IOptimismPortal2AnchorStateRegistry {
    error NotOwner();
    error InvalidOwner(address owner);
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
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);
    event FactoryInitialized(address indexed factory);
    event PausedSet(bool paused);
    event GameBlacklistedSet(address indexed game, bool blacklisted);
    event RespectedGameTypeSet(GameType gameType);

    /// @notice Post-resolution airgap before a game may anchor or finalize Portal withdrawals.
    uint256 internal immutable DISPUTE_GAME_FINALITY_DELAY_SECONDS;

    address public owner;
    address public override disputeGameFactory;
    bool public paused;
    GameType public override respectedGameType;

    Proposal private startingAnchorRoot;
    address public anchorGame;

    mapping(address game => bool blacklisted) public blacklistedGames;

    constructor(bytes32 startingRootClaim, uint256 startingL2BlockNumber, uint256 disputeGameFinalityDelaySeconds_) {
        owner = msg.sender;
        emit OwnershipTransferred(address(0), msg.sender);
        startingAnchorRoot = Proposal({root: Hash.wrap(startingRootClaim), l2SequenceNumber: startingL2BlockNumber});
        DISPUTE_GAME_FINALITY_DELAY_SECONDS = disputeGameFinalityDelaySeconds_;
        respectedGameType = GameTypes.WIP_1006;
    }

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    function transferOwnership(address nextOwner) external onlyOwner {
        if (nextOwner == address(0)) revert InvalidOwner(nextOwner);

        address previousOwner = owner;
        owner = nextOwner;
        emit OwnershipTransferred(previousOwner, nextOwner);
    }

    function initializeFactory(address factory) external onlyOwner {
        if (factory == address(0)) revert InvalidFactory(factory);
        if (disputeGameFactory != address(0)) revert FactoryAlreadyInitialized(disputeGameFactory);

        disputeGameFactory = factory;
        emit FactoryInitialized(factory);
    }

    function setPaused(bool nextPaused) external onlyOwner {
        paused = nextPaused;
        emit PausedSet(nextPaused);
    }

    function setGameBlacklisted(address game, bool blacklisted) external onlyOwner {
        if (blacklisted && game.code.length != 0) {
            GameStatus gameStatus;
            try IDisputeGame(game).status() returns (GameStatus status_) {
                gameStatus = status_;
            } catch {}
            if (gameStatus == GameStatus.DEFENDER_WINS) revert FinalizedGameCannotBeBlacklisted(game);
        }

        blacklistedGames[game] = blacklisted;
        emit GameBlacklistedSet(game, blacklisted);
    }

    function setRespectedGameType(GameType gameType) external onlyOwner {
        respectedGameType = gameType;
        emit RespectedGameTypeSet(gameType);
    }

    function disputeGameFinalityDelaySeconds() external view override returns (uint256) {
        return DISPUTE_GAME_FINALITY_DELAY_SECONDS;
    }

    /// @notice WIP-1006 does not currently retire games by creation timestamp.
    function retirementTimestamp() external pure override returns (uint64) {
        return 0;
    }

    /// @notice OP-compatible blacklist getter used by OptimismPortal2's legacy facade.
    function disputeGameBlacklist(address game) external view override returns (bool) {
        return blacklistedGames[game];
    }

    /// @notice OP-compatible blacklist getter used by standard registry tooling.
    function isGameBlacklisted(address game) external view returns (bool) {
        return blacklistedGames[game];
    }

    /// @notice Returns the immutable deployment anchor, which does not move with getAnchorRoot().
    function getStartingAnchorRoot() external view returns (Proposal memory) {
        return startingAnchorRoot;
    }

    function getAnchorRoot() public view returns (Hash, uint256) {
        address game = anchorGame;
        if (game == address(0)) {
            return (startingAnchorRoot.root, startingAnchorRoot.l2SequenceNumber);
        }

        IDisputeGame disputeGame = IDisputeGame(game);
        return (Hash.wrap(disputeGame.rootClaim()), disputeGame.l2SequenceNumber());
    }

    function isGameRegistered(address game) public view returns (bool) {
        if (game.code.length == 0) return false;

        address factory = disputeGameFactory;
        if (factory == address(0)) return false;

        try IDisputeGame(game).gameData() returns (GameType gameType, Claim rootClaim, bytes memory extraData) {
            (IDisputeGame registeredGame,) =
                IWorldChainProofSystemFactory(factory).games(gameType, rootClaim, extraData);
            if (address(registeredGame) != game) return false;
        } catch {
            return false;
        }

        try IWorldChainProofSystemGame(game).anchorStateRegistry() returns (address registry) {
            return registry == address(this);
        } catch {
            return false;
        }
    }

    function isGameRespected(address game) public view override returns (bool) {
        if (game.code.length == 0) return false;

        try IDisputeGame(game).wasRespectedGameTypeWhenCreated() returns (bool respected) {
            return respected;
        } catch {
            return false;
        }
    }

    function isGameResolved(address game) public view returns (bool) {
        if (game.code.length == 0) return false;

        try IDisputeGame(game).resolvedAt() returns (Timestamp resolvedAt) {
            if (Timestamp.unwrap(resolvedAt) == 0) return false;
        } catch {
            return false;
        }

        try IDisputeGame(game).status() returns (GameStatus gameStatus) {
            return gameStatus == GameStatus.DEFENDER_WINS || gameStatus == GameStatus.CHALLENGER_WINS;
        } catch {
            return false;
        }
    }

    /// @notice Returns whether the Portal may record a withdrawal proof against this game.
    /// @dev Proper does not mean the root claim is valid; finalization uses isGameClaimValid.
    function isGameProper(address game) public view override returns (bool) {
        if (!isGameRegistered(game)) return false;
        if (blacklistedGames[game]) return false;
        if (paused) return false;
        return true;
    }

    /// @notice Returns whether a game has resolved and passed the ASR finality timestamp check.
    function isGameFinalized(address game) public view returns (bool) {
        if (!isGameResolved(game)) return false;

        try IDisputeGame(game).resolvedAt() returns (Timestamp resolvedAt) {
            return block.timestamp - Timestamp.unwrap(resolvedAt) > DISPUTE_GAME_FINALITY_DELAY_SECONDS;
        } catch {
            return false;
        }
    }

    /// @notice Returns whether the Portal may finalize a withdrawal proven against this game.
    /// @dev Anchor advancement separately requires a newer L2 sequence number.
    function isGameClaimValid(address game) public view override returns (bool) {
        if (!isGameProper(game)) return false;
        if (!isGameRespected(game)) return false;
        if (!isGameFinalized(game)) return false;

        try IDisputeGame(game).status() returns (GameStatus gameStatus) {
            return gameStatus == GameStatus.DEFENDER_WINS;
        } catch {
            return false;
        }
    }

    /// @notice Advances the starting checkpoint for future games; this does not finalize Portal withdrawals.
    function setAnchorState(address game) external {
        address factory = disputeGameFactory;
        if (factory == address(0)) revert FactoryNotInitialized();
        if (paused) revert RegistryPaused();
        if (blacklistedGames[game]) revert GameBlacklisted(game);

        IWorldChainProofSystemGame proofGame = IWorldChainProofSystemGame(game);
        if (proofGame.factory() != factory) revert InvalidGameFactory(factory, proofGame.factory());
        if (proofGame.anchorStateRegistry() != address(this)) {
            revert InvalidGameRegistry(address(this), proofGame.anchorStateRegistry());
        }
        if (!isGameRegistered(game)) revert UnregisteredGame(game);

        if (!isGameClaimValid(game)) revert GameNotFinalized(game);

        // A game can only finalize after its parent has finalized, so FINALIZED recursively certifies
        // its parent chain at resolution time without walking the whole chain again here.
        uint256 nextL2BlockNumber = proofGame.l2SequenceNumber();
        (, uint256 currentL2BlockNumber) = getAnchorRoot();
        if (nextL2BlockNumber <= currentL2BlockNumber) {
            revert NonMonotonicRoot(currentL2BlockNumber, nextL2BlockNumber);
        }

        bytes32 nextRootId = proofGame.rootId();
        anchorGame = game;

        emit AnchorUpdated(game, nextRootId, proofGame.rootClaim(), nextL2BlockNumber);
    }
}
