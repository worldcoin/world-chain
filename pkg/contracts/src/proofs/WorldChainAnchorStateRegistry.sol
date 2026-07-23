// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Claim, GameStatus, GameType, Hash, Proposal, Timestamp, GameTypes} from "./DisputeTypes.sol";
import {IDisputeGame} from "./interfaces/IDisputeGame.sol";
import {IWorldChainProofSystemGame} from "./interfaces/IWorldChainProofSystemGame.sol";
import {IWorldChainProofSystemFactory} from "./interfaces/IWorldChainProofSystemFactory.sol";
import {IOptimismPortal2AnchorStateRegistry} from "./interfaces/IOptimismPortal2.sol";

contract WorldChainAnchorStateRegistry is IOptimismPortal2AnchorStateRegistry {
    error NotOwner();
    error AlreadyInitialized();
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

    // TODO: Evaluate a post-resolution ASR finality delay in a follow-up protocol ticket.
    // It is intentionally zero for the current WIP-1006 Portal integration.
    uint256 internal constant DISPUTE_GAME_FINALITY_DELAY_SECONDS = 0;

    /// @custom:storage-location erc7201:worldchain.storage.WorldChainAnchorStateRegistry
    struct RegistryStorage {
        address owner;
        address disputeGameFactory;
        bool paused;
        GameType respectedGameType;
        Proposal startingAnchorRoot;
        address anchorGame;
        mapping(address game => bool blacklisted) blacklistedGames;
    }

    // Isolates World Chain state from the implementation previously used by the OP-deployed proxy.
    bytes32 private constant REGISTRY_STORAGE_LOCATION =
        0x1a10faa84117cdd4e9160b243ebd4a784f48ab33f31bcfb06e1ea06aef377500;

    constructor(bytes32 startingRootClaim, uint256 startingL2BlockNumber) {
        _initialize(startingRootClaim, startingL2BlockNumber, msg.sender);
    }

    modifier onlyOwner() {
        if (msg.sender != _getRegistryStorage().owner) revert NotOwner();
        _;
    }

    /// @notice Initializes the implementation after an atomic ProxyAdmin upgrade.
    function initialize(bytes32 startingRootClaim, uint256 startingL2BlockNumber, address factory, address initialOwner)
        external
    {
        _initialize(startingRootClaim, startingL2BlockNumber, initialOwner);
        _initializeFactory(factory);
    }

    function owner() external view returns (address) {
        return _getRegistryStorage().owner;
    }

    function disputeGameFactory() public view override returns (address) {
        return _getRegistryStorage().disputeGameFactory;
    }

    function paused() public view returns (bool) {
        return _getRegistryStorage().paused;
    }

    function respectedGameType() public view override returns (GameType) {
        return _getRegistryStorage().respectedGameType;
    }

    function anchorGame() public view returns (address) {
        return _getRegistryStorage().anchorGame;
    }

    function blacklistedGames(address game) public view returns (bool) {
        return _getRegistryStorage().blacklistedGames[game];
    }

    function transferOwnership(address nextOwner) external onlyOwner {
        if (nextOwner == address(0)) revert InvalidOwner(nextOwner);

        RegistryStorage storage registryStorage = _getRegistryStorage();
        address previousOwner = registryStorage.owner;
        registryStorage.owner = nextOwner;
        emit OwnershipTransferred(previousOwner, nextOwner);
    }

    function initializeFactory(address factory) external onlyOwner {
        _initializeFactory(factory);
    }

    function setPaused(bool nextPaused) external onlyOwner {
        _getRegistryStorage().paused = nextPaused;
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

        _getRegistryStorage().blacklistedGames[game] = blacklisted;
        emit GameBlacklistedSet(game, blacklisted);
    }

    function setRespectedGameType(GameType gameType) external onlyOwner {
        _getRegistryStorage().respectedGameType = gameType;
        emit RespectedGameTypeSet(gameType);
    }

    function disputeGameFinalityDelaySeconds() external pure override returns (uint256) {
        return DISPUTE_GAME_FINALITY_DELAY_SECONDS;
    }

    /// @notice WIP-1006 does not currently retire games by creation timestamp.
    function retirementTimestamp() external pure override returns (uint64) {
        return 0;
    }

    /// @notice OP-compatible blacklist getter used by OptimismPortal2's legacy facade.
    function disputeGameBlacklist(address game) external view override returns (bool) {
        return _getRegistryStorage().blacklistedGames[game];
    }

    /// @notice OP-compatible blacklist getter used by standard registry tooling.
    function isGameBlacklisted(address game) external view returns (bool) {
        return _getRegistryStorage().blacklistedGames[game];
    }

    /// @notice Returns the immutable deployment anchor, which does not move with getAnchorRoot().
    function getStartingAnchorRoot() external view returns (Proposal memory) {
        return _getRegistryStorage().startingAnchorRoot;
    }

    function getAnchorRoot() public view returns (Hash, uint256) {
        RegistryStorage storage registryStorage = _getRegistryStorage();
        address game = registryStorage.anchorGame;
        if (game == address(0)) {
            return (registryStorage.startingAnchorRoot.root, registryStorage.startingAnchorRoot.l2SequenceNumber);
        }

        IDisputeGame disputeGame = IDisputeGame(game);
        return (Hash.wrap(disputeGame.rootClaim()), disputeGame.l2SequenceNumber());
    }

    function isGameRegistered(address game) public view returns (bool) {
        if (game.code.length == 0) return false;

        address factory = _getRegistryStorage().disputeGameFactory;
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
        RegistryStorage storage registryStorage = _getRegistryStorage();
        if (!isGameRegistered(game)) return false;
        if (registryStorage.blacklistedGames[game]) return false;
        if (registryStorage.paused) return false;
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
        RegistryStorage storage registryStorage = _getRegistryStorage();
        address factory = registryStorage.disputeGameFactory;
        if (factory == address(0)) revert FactoryNotInitialized();
        if (registryStorage.paused) revert RegistryPaused();
        if (registryStorage.blacklistedGames[game]) revert GameBlacklisted(game);

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
        registryStorage.anchorGame = game;

        emit AnchorUpdated(game, nextRootId, proofGame.rootClaim(), nextL2BlockNumber);
    }

    function _initialize(bytes32 startingRootClaim, uint256 startingL2BlockNumber, address initialOwner) internal {
        RegistryStorage storage registryStorage = _getRegistryStorage();
        if (registryStorage.owner != address(0)) revert AlreadyInitialized();
        if (initialOwner == address(0)) revert InvalidOwner(initialOwner);

        registryStorage.owner = initialOwner;
        registryStorage.startingAnchorRoot =
            Proposal({root: Hash.wrap(startingRootClaim), l2SequenceNumber: startingL2BlockNumber});
        registryStorage.respectedGameType = GameTypes.WIP_1006;

        emit OwnershipTransferred(address(0), initialOwner);
    }

    function _initializeFactory(address factory) internal {
        if (factory == address(0)) revert InvalidFactory(factory);

        RegistryStorage storage registryStorage = _getRegistryStorage();
        if (registryStorage.disputeGameFactory != address(0)) {
            revert FactoryAlreadyInitialized(registryStorage.disputeGameFactory);
        }

        registryStorage.disputeGameFactory = factory;
        emit FactoryInitialized(factory);
    }

    function _getRegistryStorage() private pure returns (RegistryStorage storage registryStorage) {
        bytes32 location = REGISTRY_STORAGE_LOCATION;
        assembly {
            registryStorage.slot := location
        }
    }
}
