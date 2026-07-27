// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {WorldChainProofLib} from "./WorldChainProofLib.sol";
import {WorldChainGameTypes} from "./WorldChainGameTypes.sol";
import {IWorldChainProofSystemGame} from "./interfaces/IWorldChainProofSystemGame.sol";
import {IWorldChainProofVerifier} from "./interfaces/IWorldChainProofVerifier.sol";
import {IWorldChainStakingRegistry} from "./interfaces/IWorldChainStakingRegistry.sol";

import {Clone} from "@solady/utils/Clone.sol";
import {
    BondDistributionMode,
    Claim,
    GameStatus,
    GameType,
    Hash,
    Timestamp
} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {
    AlreadyInitialized,
    BadExtraData,
    BondTransferFailed,
    ClaimAlreadyChallenged,
    ClaimAlreadyResolved,
    GameNotFinalized,
    GameNotOver,
    GameNotResolved,
    GamePaused,
    IncorrectBondAmount,
    InvalidBondDistributionMode,
    InvalidParentGame,
    NoCreditToClaim,
    ParentGameNotResolved,
    UnexpectedGameType,
    UnexpectedRootClaim,
    UnknownChainId
} from "@optimism-bedrock/src/dispute/lib/Errors.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";
import {IDisputeGameFactory} from "@optimism-bedrock/interfaces/dispute/IDisputeGameFactory.sol";
import {IAnchorStateRegistry} from "@optimism-bedrock/interfaces/dispute/IAnchorStateRegistry.sol";
import {IDelayedWETH} from "@optimism-bedrock/interfaces/dispute/IDelayedWETH.sol";
import {ISemver} from "@optimism-bedrock/interfaces/universal/ISemver.sol";

/// @title WorldChainProofSystemGame
/// @notice A multi-proof dispute game created through the stock Optimism `DisputeGameFactory`
///         using the Clone-With-Immutable-Args (CWIA) pattern. Proposals chain parent-to-parent
///         at a fixed block interval; a challenged proposal finalizes only once enough
///         independent proof lanes (validity proof, TEE attestation, security council) support
///         it. Bond custody uses `DelayedWETH` with the two-phase unlock/withdraw claim flow.
/// @dev Implements the `IDisputeGame` ABI without inheriting the interface: `IDisputeGame`
///      declares `rootClaimByChainId` (among others) as `pure`, while this implementation
///      reads a constructor immutable (`view`). `FaultDisputeGame` takes the same approach.
///      Structure follows `ZKDisputeGame`; challenge/lane semantics are World Chain specific.
contract WorldChainProofSystemGame is Clone, ISemver {
    ////////////////////////////////////////////////////////////////
    //                         Structs                            //
    ////////////////////////////////////////////////////////////////

    /// @notice Per-deployment configuration, fixed as immutables on the implementation.
    /// @dev The implementation is registered with empty DGF implementation args, so none of
    ///      this configuration rides in the CWIA payload.
    struct GameConfig {
        WorldChainProofLib.Domain domain;
        uint64 challengePeriod;
        uint64 proofPeriod;
        uint256 proposerBond;
        uint256 challengerBond;
        uint8 proofThreshold;
        IWorldChainProofVerifier validityProofVerifier;
        IWorldChainProofVerifier teeVerifier;
        IWorldChainProofVerifier securityCouncil;
        IWorldChainStakingRegistry stakingRegistry;
        IDisputeGameFactory disputeGameFactory;
        IAnchorStateRegistry anchorStateRegistry;
        IDelayedWETH weth;
    }

    ////////////////////////////////////////////////////////////////
    //                         Errors                             //
    ////////////////////////////////////////////////////////////////

    error InvalidActivationParameters();
    error NotDisputeGameFactory(address caller);
    error AnchorRootNotFound();
    error InvalidL2BlockNumber(uint256 expectedL2BlockNumber, uint256 actualL2BlockNumber);
    error GameNotRetryable(bytes32 uuidPreimageHash);
    error UnstakedChallenger(address challenger);
    error ChallengePeriodElapsed(uint256 timestamp, uint256 challengeDeadline);
    error ProofPeriodElapsed(uint256 timestamp, uint256 proofDeadline);
    error InvalidLane(uint8 lane);
    error InvalidProof(WorldChainProofLib.ProofLane lane, bytes32 rootId);
    error InvalidDomainHash(bytes32 expected, bytes32 actual);
    error InconsistentSystemConfiguration();

    ////////////////////////////////////////////////////////////////
    //                         Events                             //
    ////////////////////////////////////////////////////////////////

    /// @notice Emitted at creation with the full proposal context. Replaces the former
    ///         factory `GameCreated` event for offchain indexers; the stock factory's
    ///         `DisputeGameCreated` event only carries (proxy, gameType, rootClaim).
    event WorldChainGameCreated(
        bytes32 indexed rootId,
        address indexed parentRef,
        bytes32 rootClaim,
        uint256 l2BlockNumber,
        bytes32 l1OriginHash,
        uint256 l1OriginNumber,
        uint256 attempt,
        address gameCreator
    );

    /// @notice Emitted when the game is resolved. Matches the `IDisputeGame` event.
    event Resolved(GameStatus indexed status);

    event Challenged(address indexed challenger, uint64 proofDeadline);
    event ProofLaneSupported(WorldChainProofLib.ProofLane indexed lane, bytes32 indexed rootId, uint8 proofBitmap);
    event ProofThresholdReached(bytes32 indexed rootId, uint8 proofBitmap);
    event DuplicateProofLane(WorldChainProofLib.ProofLane indexed lane, bytes32 indexed rootId, uint8 proofBitmap);
    event GameClosed(BondDistributionMode bondDistributionMode);

    ////////////////////////////////////////////////////////////////
    //                       Immutables                           //
    ////////////////////////////////////////////////////////////////

    /// Number of distinct proof lanes required to finalize a challenged root.
    uint8 public immutable PROOF_THRESHOLD;
    uint8 public constant PROOF_LANE_COUNT = WorldChainProofLib.PROOF_LANE_COUNT;

    uint256 internal immutable DOMAIN_CHAIN_ID;
    uint256 internal immutable DOMAIN_PROOF_SYSTEM_VERSION;
    bytes32 internal immutable DOMAIN_ROLLUP_CONFIG_HASH;
    uint256 internal immutable DOMAIN_BLOCK_INTERVAL;
    bytes32 public immutable domainHash;

    uint64 public immutable challengePeriod;
    uint64 public immutable proofPeriod;
    uint256 public immutable proposerBond;
    uint256 public immutable challengerBond;

    IWorldChainProofVerifier public immutable validityProofVerifier;
    IWorldChainProofVerifier public immutable teeVerifier;
    IWorldChainProofVerifier public immutable securityCouncil;
    IWorldChainStakingRegistry public immutable stakingRegistry;
    IDisputeGameFactory public immutable disputeGameFactory;
    IAnchorStateRegistry public immutable anchorStateRegistry;
    IDelayedWETH public immutable weth;

    ////////////////////////////////////////////////////////////////
    //                         Storage                            //
    ////////////////////////////////////////////////////////////////

    /// @notice Semantic version.
    /// @custom:semver 1.0.0
    string public constant version = "1.0.0";

    Timestamp public createdAt;
    Timestamp public resolvedAt;
    GameStatus public status;
    bool internal initialized;

    /// @notice The proposal transition identifier bound by every proof lane.
    bytes32 public rootId;
    bytes32 public startingRootClaim;
    uint256 public startingL2BlockNumber;
    uint64 internal _l1OriginNumber;

    uint64 public challengeDeadline;
    uint64 public challengedAt;
    uint64 public proofDeadline;
    address payable public challenger;
    uint8 public proofBitmap;
    WorldChainProofLib.InvalidationReason public invalidationReason;

    mapping(address recipient => uint256 amount) public normalModeCredit;
    mapping(address recipient => uint256 amount) public refundModeCredit;
    uint256 public totalBonds;
    BondDistributionMode public bondDistributionMode;
    bool public wasRespectedGameTypeWhenCreated;

    constructor(GameConfig memory config) {
        if (
            config.challengePeriod == 0 || config.proofPeriod <= config.challengePeriod || config.domain.chainId == 0
                || config.domain.proofSystemVersion == 0 || config.domain.blockInterval == 0
                || config.proofThreshold == 0 || config.proofThreshold > WorldChainProofLib.PROOF_LANE_COUNT
                || address(config.disputeGameFactory) == address(0) || address(config.anchorStateRegistry) == address(0)
                || address(config.weth) == address(0) || address(config.stakingRegistry) == address(0)
                || address(config.validityProofVerifier) == address(0) || address(config.teeVerifier) == address(0)
                || address(config.securityCouncil) == address(0)
        ) {
            revert InvalidActivationParameters();
        }
        if (
            address(config.anchorStateRegistry.disputeGameFactory()) != address(config.disputeGameFactory)
                || address(config.weth.systemConfig()) != address(config.anchorStateRegistry.systemConfig())
                || config.domain.chainId != config.anchorStateRegistry.systemConfig().l2ChainId()
        ) {
            revert InconsistentSystemConfiguration();
        }

        DOMAIN_CHAIN_ID = config.domain.chainId;
        DOMAIN_PROOF_SYSTEM_VERSION = config.domain.proofSystemVersion;
        DOMAIN_ROLLUP_CONFIG_HASH = config.domain.rollupConfigHash;
        DOMAIN_BLOCK_INTERVAL = config.domain.blockInterval;
        domainHash = WorldChainProofLib.domainHash(config.domain);
        challengePeriod = config.challengePeriod;
        proofPeriod = config.proofPeriod;
        proposerBond = config.proposerBond;
        challengerBond = config.challengerBond;
        PROOF_THRESHOLD = config.proofThreshold;
        validityProofVerifier = config.validityProofVerifier;
        teeVerifier = config.teeVerifier;
        securityCouncil = config.securityCouncil;
        stakingRegistry = config.stakingRegistry;
        disputeGameFactory = config.disputeGameFactory;
        anchorStateRegistry = config.anchorStateRegistry;
        weth = config.weth;
    }

    ////////////////////////////////////////////////////////////////
    //                       CWIA getters                         //
    ////////////////////////////////////////////////////////////////

    // CWIA layout appended by `DisputeGameFactory.create` (no implementation args):
    //   [0x00, 0x14) creator address
    //   [0x14, 0x34) root claim
    //   [0x34, 0x54) l1 head (parent block hash at creation)
    //   [0x54, 0xD4) extraData = abi.encode(domainHash, l2BlockNumber, parentRef, attempt)

    function gameCreator() public pure returns (address creator_) {
        creator_ = _getArgAddress(0x00);
    }

    function rootClaim() public pure returns (Claim rootClaim_) {
        rootClaim_ = Claim.wrap(_getArgBytes32(0x14));
    }

    function l1Head() public pure returns (Hash l1Head_) {
        l1Head_ = Hash.wrap(_getArgBytes32(0x34));
    }

    function proposalDomainHash() public pure returns (bytes32 domainHash_) {
        domainHash_ = _getArgBytes32(0x54);
    }

    /// @notice The L2 block number of the output root claimed by this proposal.
    function l2SequenceNumber() public pure returns (uint256 l2SequenceNumber_) {
        l2SequenceNumber_ = _getArgUint256(0x74);
    }

    /// @notice Parent game, or the anchor registry when the proposal starts from its current root.
    function parentRef() public pure returns (address parentRef_) {
        uint256 rawParentRef = _getArgUint256(0x94);
        if (rawParentRef > type(uint160).max) revert BadExtraData();
        parentRef_ = address(uint160(rawParentRef));
    }

    /// @notice Retry nonce for this transition. Attempt N requires attempt N-1 to have timed
    ///         out on proofs or to have been created before this game type became respected.
    function attempt() public pure returns (uint256 attempt_) {
        attempt_ = _getArgUint256(0xB4);
    }

    function extraData() public pure returns (bytes memory extraData_) {
        extraData_ = _getArgBytes(0x54, 0x80);
    }

    ////////////////////////////////////////////////////////////////
    //                  `IDisputeGame` surface                    //
    ////////////////////////////////////////////////////////////////

    function gameType() public pure returns (GameType gameType_) {
        gameType_ = WorldChainGameTypes.WIP_1006;
    }

    function rootClaimByChainId(uint256 chainId) external view returns (Claim rootClaim_) {
        if (chainId != DOMAIN_CHAIN_ID) revert UnknownChainId();
        rootClaim_ = rootClaim();
    }

    function gameData() external pure returns (GameType gameType_, Claim rootClaim_, bytes memory extraData_) {
        gameType_ = gameType();
        rootClaim_ = rootClaim();
        extraData_ = extraData();
    }

    ////////////////////////////////////////////////////////////////
    //                     Legacy-named views                     //
    ////////////////////////////////////////////////////////////////

    /// @notice Domain parameters this deployment proves against.
    function domain() external view returns (WorldChainProofLib.Domain memory) {
        return WorldChainProofLib.Domain({
            chainId: DOMAIN_CHAIN_ID,
            proofSystemVersion: DOMAIN_PROOF_SYSTEM_VERSION,
            rollupConfigHash: DOMAIN_ROLLUP_CONFIG_HASH,
            blockInterval: DOMAIN_BLOCK_INTERVAL
        });
    }

    /// @notice Alias of `l2SequenceNumber` retained for proof-lane and offchain consumers.
    function l2BlockNumber() external pure returns (uint256) {
        return l2SequenceNumber();
    }

    /// @notice Alias of `l1Head` retained for proof-lane and offchain consumers.
    function l1OriginHash() external pure returns (bytes32) {
        return Hash.unwrap(l1Head());
    }

    function l1OriginNumber() external view returns (uint256) {
        return _l1OriginNumber;
    }

    /// @notice Derived legacy state machine view.
    function state() public view returns (WorldChainProofLib.RootState) {
        if (status == GameStatus.DEFENDER_WINS) return WorldChainProofLib.RootState.FINALIZED;
        if (status == GameStatus.CHALLENGER_WINS) return WorldChainProofLib.RootState.INVALIDATED;
        return
            challenger == address(0) ? WorldChainProofLib.RootState.PROPOSED : WorldChainProofLib.RootState.CHALLENGED;
    }

    function finalizedAt() external view returns (uint64) {
        return status == GameStatus.DEFENDER_WINS ? resolvedAt.raw() : 0;
    }

    function invalidatedAt() external view returns (uint64) {
        return status == GameStatus.CHALLENGER_WINS ? resolvedAt.raw() : 0;
    }

    function proofCount() external view returns (uint8) {
        return WorldChainProofLib.proofCount(proofBitmap);
    }

    ////////////////////////////////////////////////////////////////
    //                     Initialization                         //
    ////////////////////////////////////////////////////////////////

    /// @notice Initializes a WIP-1006 clone and validates its domain, parent, interval, retry,
    ///         and bond invariants. Any revert bubbles through `DisputeGameFactory.create`.
    function initialize() external payable {
        if (initialized) revert AlreadyInitialized();

        // Reject any calldata whose length differs from the exact CWIA payload. This prevents
        // extraData padding games that would mint distinct factory UUIDs for the same proposal,
        // and blocks direct `initialize` calls on clones with malformed payloads.
        //
        // Expected length: 0xDA
        // - 0x04 selector
        // - 0x14 creator address
        // - 0x20 root claim
        // - 0x20 l1 head
        // - 0x80 extraData (domainHash, l2BlockNumber, parentRef, attempt)
        // - 0x02 CWIA length suffix
        if (msg.data.length != 0xDA) revert BadExtraData();

        // Only the configured factory may initialize; this also rules out direct initialization
        // of the implementation contract itself.
        if (msg.sender != address(disputeGameFactory)) revert NotDisputeGameFactory(msg.sender);

        // Defense-in-depth against `setInitBond` drifting from the configured proposer bond.
        if (msg.value != proposerBond) revert IncorrectBondAmount();

        // Preserves the former propose-time registry pause gate.
        if (anchorStateRegistry.paused()) revert GamePaused();
        if (proposalDomainHash() != domainHash) revert InvalidDomainHash(domainHash, proposalDomainHash());

        (Hash anchorRoot, uint256 anchorL2BlockNumber) = anchorStateRegistry.getAnchorRoot();
        address parentRef_ = parentRef();

        if (parentRef_ == address(anchorStateRegistry)) {
            // The proposal extends the accepted anchor; the registry acts as the parent sentinel.
            if (Hash.unwrap(anchorRoot) == bytes32(0)) revert AnchorRootNotFound();
            startingRootClaim = Hash.unwrap(anchorRoot);
            startingL2BlockNumber = anchorL2BlockNumber;
        } else {
            if (parentRef_.code.length == 0) revert InvalidParentGame();
            IDisputeGame parent = IDisputeGame(parentRef_);
            (GameType parentType, Claim parentClaim, bytes memory parentExtraData) = parent.gameData();
            (IDisputeGame registeredParent,) = disputeGameFactory.games(parentType, parentClaim, parentExtraData);

            if (address(registeredParent) != parentRef_) revert InvalidParentGame();
            if (parentType.raw() != WorldChainGameTypes.WIP_1006.raw()) revert UnexpectedGameType();
            if (parent.status() == GameStatus.CHALLENGER_WINS) revert InvalidParentGame();
            if (anchorStateRegistry.isGameBlacklisted(parent) || anchorStateRegistry.isGameRetired(parent)) {
                revert InvalidParentGame();
            }
            if (!parent.wasRespectedGameTypeWhenCreated()) revert InvalidParentGame();
            // Guards against chaining onto games from an older implementation with a different
            // domain (e.g. after a proof-system version bump reusing the same game type).
            if (IWorldChainProofSystemGame(address(parent)).domainHash() != domainHash) revert InvalidParentGame();

            startingRootClaim = Claim.unwrap(parent.rootClaim());
            startingL2BlockNumber = parent.l2SequenceNumber();

            // A parent at or below the anchor is stale: proposals extending the anchor state
            // must use the anchor sentinel instead so their starting root is registry-attested.
            if (startingL2BlockNumber <= anchorL2BlockNumber) revert InvalidParentGame();
        }

        // TODO(PROTO-4907): Confirm whether proposals require an exact block interval or only
        // a bounded range.
        uint256 expectedL2BlockNumber = startingL2BlockNumber + DOMAIN_BLOCK_INTERVAL;
        if (l2SequenceNumber() != expectedL2BlockNumber) {
            revert InvalidL2BlockNumber(expectedL2BlockNumber, l2SequenceNumber());
        }
        // Per spec, the sequence number must fit within a uint64.
        if (l2SequenceNumber() > type(uint64).max) revert UnexpectedRootClaim(rootClaim());

        // Retries: attempt N is only proposable when attempt N-1 for the identical transition
        // timed out on proofs or was created before WIP-1006 became respected. The latter
        // prevents a pre-cutover game from permanently occupying the factory UUID. Inherited
        // invalidations must rebase onto a replacement parent, which changes `parentRef` and
        // therefore starts back at attempt zero. Duplicate attempts are impossible: the factory
        // UUID covers (gameType, rootClaim, extraData).
        if (attempt() > 0) {
            bytes memory previousExtraData = abi.encode(domainHash, l2SequenceNumber(), parentRef_, attempt() - 1);
            (IDisputeGame previous,) =
                disputeGameFactory.games(WorldChainGameTypes.WIP_1006, rootClaim(), previousExtraData);
            if (
                address(previous) == address(0)
                    || (previous.wasRespectedGameTypeWhenCreated()
                        && (previous.status() != GameStatus.CHALLENGER_WINS
                            || IWorldChainProofSystemGame(address(previous)).invalidationReason()
                                != WorldChainProofLib.InvalidationReason.PROOF_TIMEOUT))
            ) {
                revert GameNotRetryable(keccak256(
                        abi.encode(WorldChainGameTypes.WIP_1006, rootClaim(), previousExtraData)
                    ));
            }
        }

        // `initialize` runs in the same transaction as `DisputeGameFactory.create`, which set
        // `l1Head = blockhash(block.number - 1)`.
        _l1OriginNumber = uint64(block.number - 1);
        rootId = WorldChainProofLib.rootId(
            domainHash,
            parentRef_,
            Claim.unwrap(rootClaim()),
            l2SequenceNumber(),
            Hash.unwrap(l1Head()),
            _l1OriginNumber
        );

        challengeDeadline = uint64(block.timestamp + challengePeriod);
        proofDeadline = uint64(block.timestamp + proofPeriod);
        createdAt = Timestamp.wrap(uint64(block.timestamp));
        initialized = true;

        // Custody the proposer bond in DelayedWETH and track the refund-mode credit.
        refundModeCredit[gameCreator()] += msg.value;
        totalBonds += msg.value;
        weth.deposit{value: msg.value}();

        wasRespectedGameTypeWhenCreated =
            anchorStateRegistry.respectedGameType().raw() == WorldChainGameTypes.WIP_1006.raw();

        emit WorldChainGameCreated(
            rootId,
            parentRef_,
            Claim.unwrap(rootClaim()),
            l2SequenceNumber(),
            Hash.unwrap(l1Head()),
            _l1OriginNumber,
            attempt(),
            gameCreator()
        );
    }

    ////////////////////////////////////////////////////////////////
    //                    Challenge and proofs                    //
    ////////////////////////////////////////////////////////////////

    function challenge() external payable {
        if (status != GameStatus.IN_PROGRESS) revert ClaimAlreadyResolved();
        if (challenger != address(0)) revert ClaimAlreadyChallenged();
        if (block.timestamp >= challengeDeadline) {
            revert ChallengePeriodElapsed(block.timestamp, challengeDeadline);
        }
        if (!stakingRegistry.isStaked(msg.sender)) revert UnstakedChallenger(msg.sender);
        if (msg.value != challengerBond) revert IncorrectBondAmount();

        challenger = payable(msg.sender);
        challengedAt = uint64(block.timestamp);

        // Custody the challenger bond in DelayedWETH and track the refund-mode credit.
        refundModeCredit[msg.sender] += msg.value;
        totalBonds += msg.value;
        weth.deposit{value: msg.value}();

        emit Challenged(msg.sender, proofDeadline);
    }

    function submitProofLane(uint8 laneId, bytes calldata proof) external {
        if (status != GameStatus.IN_PROGRESS || challenger == address(0)) {
            revert ClaimAlreadyResolved();
        }
        if (block.timestamp >= proofDeadline) {
            revert ProofPeriodElapsed(block.timestamp, proofDeadline);
        }
        if (laneId >= PROOF_LANE_COUNT) revert InvalidLane(laneId);

        WorldChainProofLib.ProofLane lane = WorldChainProofLib.ProofLane(laneId);
        uint8 mask = WorldChainProofLib.laneMask(lane);
        if ((proofBitmap & mask) != 0) {
            emit DuplicateProofLane(lane, rootId, proofBitmap);
            return;
        }

        if (!_verifierFor(lane).verify(rootId, proof)) {
            revert InvalidProof(lane, rootId);
        }

        bool thresholdAlreadyReached = WorldChainProofLib.hasThreshold(proofBitmap, PROOF_THRESHOLD);
        proofBitmap |= mask;
        emit ProofLaneSupported(lane, rootId, proofBitmap);

        // Emit only on the transition to settlement-ready so offchain consumers receive a single signal.
        if (!thresholdAlreadyReached && WorldChainProofLib.hasThreshold(proofBitmap, PROOF_THRESHOLD)) {
            emit ProofThresholdReached(rootId, proofBitmap);
        }
    }

    ////////////////////////////////////////////////////////////////
    //                        Resolution                          //
    ////////////////////////////////////////////////////////////////

    /// @notice Returns whether this game can resolve now and the resulting legacy outcome.
    function resolutionStatus()
        external
        view
        returns (bool resolvable, WorldChainProofLib.RootState outcome, WorldChainProofLib.InvalidationReason reason)
    {
        if (status != GameStatus.IN_PROGRESS) {
            return (false, state(), invalidationReason);
        }

        (GameStatus parentStatus, bool parentBlacklisted) = _parentResolution();
        if (parentBlacklisted || parentStatus == GameStatus.CHALLENGER_WINS) {
            return
                (true, WorldChainProofLib.RootState.INVALIDATED, WorldChainProofLib.InvalidationReason.INVALID_PARENT);
        }
        if (parentStatus == GameStatus.IN_PROGRESS) {
            return (false, state(), WorldChainProofLib.InvalidationReason.NONE);
        }

        if (challenger == address(0)) {
            return block.timestamp >= challengeDeadline
                ? (true, WorldChainProofLib.RootState.FINALIZED, WorldChainProofLib.InvalidationReason.NONE)
                : (false, WorldChainProofLib.RootState.PROPOSED, WorldChainProofLib.InvalidationReason.NONE);
        }

        if (WorldChainProofLib.hasThreshold(proofBitmap, PROOF_THRESHOLD)) {
            return (true, WorldChainProofLib.RootState.FINALIZED, WorldChainProofLib.InvalidationReason.NONE);
        }
        return block.timestamp >= proofDeadline
            ? (true, WorldChainProofLib.RootState.INVALIDATED, WorldChainProofLib.InvalidationReason.PROOF_TIMEOUT)
            : (false, WorldChainProofLib.RootState.CHALLENGED, WorldChainProofLib.InvalidationReason.NONE);
    }

    /// @notice Resolves the game.
    ///         `DEFENDER_WINS` when the challenge window expires unchallenged, or when enough
    ///         proof lanes support a challenged claim. `CHALLENGER_WINS` when the proof window
    ///         expires below threshold, or when the parent game is invalid.
    /// @dev Resolution gates on the parent's *status*, never on its claim validity, so the
    ///      anchor registry's finality airgap does not slow the proposal cadence. Bonds are
    ///      credited here and paid out through `claimCredit` after `closeGame`.
    function resolve() external returns (GameStatus status_) {
        if (status != GameStatus.IN_PROGRESS) revert ClaimAlreadyResolved();

        (GameStatus parentStatus, bool parentBlacklisted) = _parentResolution();

        if (parentBlacklisted || parentStatus == GameStatus.CHALLENGER_WINS) {
            // An invalid parent invalidates this game regardless of its own proof state. Unlike
            // ZKDisputeGame (which awards the challenger), both bonds are refunded: neither
            // party is at fault for an ancestor's failure.
            status = GameStatus.CHALLENGER_WINS;
            invalidationReason = WorldChainProofLib.InvalidationReason.INVALID_PARENT;
            normalModeCredit[gameCreator()] += proposerBond;
            if (challenger != address(0)) normalModeCredit[challenger] += challengerBond;
        } else if (parentStatus == GameStatus.IN_PROGRESS) {
            // A proposed or challenged parent must resolve before its descendant.
            revert ParentGameNotResolved();
        } else if (challenger == address(0)) {
            // An unchallenged proposal finalizes after its challenge window expires. Safety
            // therefore relies on every incorrect claim being challenged before this deadline.
            if (block.timestamp < challengeDeadline) revert GameNotOver();
            status = GameStatus.DEFENDER_WINS;
            normalModeCredit[gameCreator()] = totalBonds;
        } else if (WorldChainProofLib.hasThreshold(proofBitmap, PROOF_THRESHOLD)) {
            // A challenged game finalizes as soon as enough independent proof lanes support it.
            // The proposer takes the challenger bond.
            status = GameStatus.DEFENDER_WINS;
            normalModeCredit[gameCreator()] = totalBonds;
        } else if (block.timestamp >= proofDeadline) {
            // A challenged game below threshold times out once its proof window expires. The
            // challenger takes the proposer bond.
            status = GameStatus.CHALLENGER_WINS;
            invalidationReason = WorldChainProofLib.InvalidationReason.PROOF_TIMEOUT;
            normalModeCredit[challenger] = totalBonds;
        } else {
            revert GameNotOver();
        }

        resolvedAt = Timestamp.wrap(uint64(block.timestamp));
        emit Resolved(status);

        return status;
    }

    /// @notice Returns the parent's resolution inputs.
    /// @dev The anchor sentinel counts as a finalized parent: the anchor is only ever set from
    ///      a claim-valid game, so its root is already trusted. Parent blacklist evaluation is
    ///      retained from the legacy game (stock ZKDisputeGame leaves it to the guardian): a
    ///      blacklisted parent invalidates descendants at resolution without further guardian
    ///      action, even while that parent is unresolved.
    function _parentResolution() internal view returns (GameStatus parentStatus, bool parentBlacklisted) {
        address parentRef_ = parentRef();
        if (parentRef_ == address(anchorStateRegistry)) {
            return (GameStatus.DEFENDER_WINS, false);
        }
        IDisputeGame parent = IDisputeGame(parentRef_);
        parentBlacklisted = anchorStateRegistry.isGameBlacklisted(parent);
        if (!parentBlacklisted) parentStatus = parent.status();
    }

    ////////////////////////////////////////////////////////////////
    //                      Bond settlement                       //
    ////////////////////////////////////////////////////////////////

    /// @notice Closes out the game: requires the registry to consider it finalized (resolution
    ///         plus the finality airgap), attempts to advance the anchor to it, and locks in
    ///         the bond distribution mode (`REFUND` for improper games — blacklisted, retired,
    ///         or otherwise invalidated by the registry).
    function closeGame() public {
        if (bondDistributionMode == BondDistributionMode.REFUND || bondDistributionMode == BondDistributionMode.NORMAL)
        {
            // Already closed; must not revert or `claimCredit` would break.
            return;
        } else if (bondDistributionMode != BondDistributionMode.UNDECIDED) {
            revert InvalidBondDistributionMode();
        }

        // While the system is paused games are temporarily invalid; closing now would lock in
        // refund mode spuriously.
        if (anchorStateRegistry.paused()) revert GamePaused();

        if (resolvedAt.raw() == 0) revert GameNotResolved();

        IDisputeGame self = IDisputeGame(address(this));

        // The finality airgap must elapse after resolution before any payout.
        if (!anchorStateRegistry.isGameFinalized(self)) revert GameNotFinalized();

        // Advancing the anchor is best-effort: this game may legitimately be ineligible (e.g.
        // an older block number than the current anchor, or an unrespected game type).
        // nosemgrep: sol-safety-trycatch-eip150
        try anchorStateRegistry.setAnchorState(self) {} catch {}

        bondDistributionMode =
            anchorStateRegistry.isGameProper(self) ? BondDistributionMode.NORMAL : BondDistributionMode.REFUND;

        emit GameClosed(bondDistributionMode);
    }

    /// @notice Claims the credit belonging to `recipient` using the two-phase DelayedWETH
    ///         withdrawal pattern: the first call unlocks, the second (after the WETH delay)
    ///         withdraws and transfers. Permissionless; funds only ever move to `recipient`.
    function claimCredit(address recipient) external {
        // If closeGame() flips the distribution mode within this call and there is nothing to
        // claim, return instead of reverting so the close is not rolled back.
        bool gameWasOpen = bondDistributionMode == BondDistributionMode.UNDECIDED;

        closeGame();

        uint256 recipientCredit;
        if (bondDistributionMode == BondDistributionMode.REFUND) {
            recipientCredit = refundModeCredit[recipient];
        } else if (bondDistributionMode == BondDistributionMode.NORMAL) {
            recipientCredit = normalModeCredit[recipient];
        } else {
            revert InvalidBondDistributionMode();
        }

        // Phase 1: zero the credit and unlock it in DelayedWETH.
        if (recipientCredit > 0) {
            refundModeCredit[recipient] = 0;
            normalModeCredit[recipient] = 0;
            weth.unlock(recipient, recipientCredit);
            return;
        }

        // Phase 2: finalize the pending DelayedWETH withdrawal.
        (uint256 amount,) = weth.withdrawals(address(this), recipient);
        if (amount == 0) {
            if (gameWasOpen) return;
            revert NoCreditToClaim();
        }

        weth.withdraw(recipient, amount);

        // solady's CWIA proxy implements `receive()`, so the WETH98 2300-gas transfer above
        // succeeds without a `receive()` on this implementation.
        (bool success,) = recipient.call{value: amount}(hex"");
        if (!success) revert BondTransferFailed();
    }

    /// @notice Returns the credit `recipient` will receive under the current distribution mode.
    /// @dev Before `closeGame`, registry-invalid games report refund credit so keepers do not
    ///      miss challenger refunds merely because normal-mode credit is zero.
    function credit(address recipient) external view returns (uint256 credit_) {
        if (
            bondDistributionMode == BondDistributionMode.REFUND
                || (bondDistributionMode == BondDistributionMode.UNDECIDED
                    && !anchorStateRegistry.isGameProper(IDisputeGame(address(this))))
        ) {
            credit_ = refundModeCredit[recipient];
        } else {
            credit_ = normalModeCredit[recipient];
        }
    }

    function _verifierFor(WorldChainProofLib.ProofLane lane) internal view returns (IWorldChainProofVerifier) {
        if (lane == WorldChainProofLib.ProofLane.VALIDITY_PROOF) return validityProofVerifier;
        if (lane == WorldChainProofLib.ProofLane.TEE_ATTESTATION) return teeVerifier;
        return securityCouncil;
    }
}
