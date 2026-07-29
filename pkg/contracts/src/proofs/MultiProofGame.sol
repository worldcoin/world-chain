// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ProofLib} from "./lib/ProofLib.sol";
import {GameTypes} from "./GameTypes.sol";
import {IMultiProofGame} from "./interfaces/IMultiProofGame.sol";
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
    UnexpectedRootClaim
} from "@optimism-bedrock/src/dispute/lib/Errors.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";
import {IDisputeGameFactory} from "@optimism-bedrock/interfaces/dispute/IDisputeGameFactory.sol";
import {IAnchorStateRegistry} from "@optimism-bedrock/interfaces/dispute/IAnchorStateRegistry.sol";
import {IDelayedWETH} from "@optimism-bedrock/interfaces/dispute/IDelayedWETH.sol";
import {ISemver} from "@optimism-bedrock/interfaces/universal/ISemver.sol";

/// @title MultiProofGame
/// @notice A multi-proof dispute game created through the stock Optimism `DisputeGameFactory`
///         using the Clone-With-Immutable-Args (CWIA) pattern. Proposals chain parent-to-parent
///         at a fixed block interval; a challenged proposal finalizes only once enough
///         independent proof lanes (validity proof, TEE attestation, security council) support
///         it. Bond custody uses `DelayedWETH` with the two-phase unlock/withdraw claim flow.
/// @dev Structure follows `ZKDisputeGame`; challenge/lane semantics are World Chain specific.
contract MultiProofGame is Clone, ISemver, IMultiProofGame {
    /// @dev EIP-2935 history contract, retaining the latest 8,191 block hashes.
    address internal constant HISTORY_STORAGE = 0x0000F90827F1C53a10cb7A02335B175320002935;

    ////////////////////////////////////////////////////////////////
    //                       Immutables                           //
    ////////////////////////////////////////////////////////////////

    /// @inheritdoc IMultiProofGame
    uint8 public immutable PROOF_THRESHOLD;

    /// @inheritdoc IMultiProofGame
    uint8 public constant PROOF_LANE_COUNT = ProofLib.PROOF_LANE_COUNT;

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

    /// @inheritdoc IMultiProofGame
    bytes32 public rootId;
    bytes32 public startingRootClaim;
    uint256 public startingL2BlockNumber;
    uint64 internal _l1OriginNumber;

    uint64 public challengeDeadline;
    uint64 public challengedAt;
    uint64 public proofDeadline;
    address payable public challenger;
    uint8 public proofBitmap;
    ProofLib.InvalidationReason public invalidationReason;

    mapping(address recipient => uint256 amount) public normalModeCredit;
    mapping(address recipient => uint256 amount) public refundModeCredit;
    uint256 public totalBonds;
    BondDistributionMode public bondDistributionMode;
    bool public wasRespectedGameTypeWhenCreated;

    constructor(GameConfig memory config) {
        if (
            config.challengePeriod == 0 || config.proofPeriod <= config.challengePeriod || config.domain.chainId == 0
                || config.domain.proofSystemVersion == 0 || config.domain.blockInterval == 0
                || config.proofThreshold == 0 || config.proofThreshold > ProofLib.PROOF_LANE_COUNT
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
        domainHash = ProofLib.domainHash(config.domain);
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

    // CWIA layout appended by `DisputeGameFactory.create` (no implementation args).
    // Addresses in ABI words are right-aligned:
    //   [0x000, 0x014) creator address                    (factory prefix)
    //   [0x014, 0x034) root claim                         (factory prefix)
    //   [0x034, 0x054) factory-captured parent block hash (unused by this game)
    //   [0x054, 0x074) domainHash                         (extraData word 0)
    //   [0x074, 0x094) l2BlockNumber                      (extraData word 1)
    //   [0x094, 0x0B4) parentRef                          (extraData word 2)
    //   [0x0B4, 0x0D4) attempt                            (extraData word 3)
    //   [0x0D4, 0x0F4) retryOf                            (extraData word 4)
    //   [0x0F4, 0x114) l1OriginHash                       (extraData word 5)
    //   [0x114, 0x134) l1OriginNumber                     (extraData word 6)
    //   [0x134, 0x154) creationProof offset (= 0x100)     (extraData word 7)
    //   [0x154, 0x174) creationProof length               (dynamic tail)
    //   [0x174, ...) creationProof bytes, padded to 32 bytes

    function gameCreator() public pure returns (address creator_) {
        creator_ = _getArgAddress(0x00);
    }

    function rootClaim() public pure returns (Claim rootClaim_) {
        rootClaim_ = Claim.wrap(_getArgBytes32(0x14));
    }

    function l1Head() public pure returns (Hash l1Head_) {
        l1Head_ = Hash.wrap(_getArgBytes32(0xF4));
    }

    /// @inheritdoc IMultiProofGame
    function proposalDomainHash() public pure returns (bytes32 domainHash_) {
        domainHash_ = _getArgBytes32(0x54);
    }

    /// @notice The L2 block number of the output root claimed by this proposal.
    function l2SequenceNumber() public pure returns (uint256 l2SequenceNumber_) {
        l2SequenceNumber_ = _getArgUint256(0x74);
    }

    /// @inheritdoc IMultiProofGame
    function parentRef() public pure returns (address parentRef_) {
        uint256 rawParentRef = _getArgUint256(0x94);
        if (rawParentRef > type(uint160).max) revert BadExtraData();
        // casting to 'uint160' is safe because the check above reverts on any wider value
        // forge-lint: disable-next-line(unsafe-typecast)
        parentRef_ = address(uint160(rawParentRef));
    }

    /// @inheritdoc IMultiProofGame
    function attempt() public pure returns (uint256 attempt_) {
        attempt_ = _getArgUint256(0xB4);
    }

    /// @inheritdoc IMultiProofGame
    function retryOf() public pure returns (address retryOf_) {
        uint256 rawRetryOf = _getArgUint256(0xD4);
        if (rawRetryOf > type(uint160).max) revert BadExtraData();
        // forge-lint: disable-next-line(unsafe-typecast)
        retryOf_ = address(uint160(rawRetryOf));
    }

    /// @inheritdoc IMultiProofGame
    function creationProof() public pure returns (bytes memory proof_) {
        bytes memory data = extraData();
        (,,,,,,, proof_) = abi.decode(data, (bytes32, uint256, address, uint256, address, bytes32, uint256, bytes));
    }

    /// @notice Returns the ABI-encoded proposal data supplied to `DisputeGameFactory.create`.
    /// @dev Excludes the 0x54-byte CWIA prefix containing the creator, root claim, and factory-captured L1 block hash.
    function extraData() public pure returns (bytes memory extraData_) {
        bytes memory args = _getArgBytes();
        if (args.length < 0x54) revert BadExtraData();
        extraData_ = _getArgBytes(0x54, args.length - 0x54);
    }

    ////////////////////////////////////////////////////////////////
    //                  `IDisputeGame` surface                    //
    ////////////////////////////////////////////////////////////////

    function gameType() public pure returns (GameType gameType_) {
        gameType_ = GameTypes.MULTI_PROOF_GAME_TYPE;
    }

    /// @dev `IDisputeGame` declares this `pure`, so it cannot gate on the `DOMAIN_CHAIN_ID`
    ///      immutable. This matches `ZKDisputeGame`; only super game types carry per-chain
    ///      root claims, and `OptimismPortal2` reads `rootClaim()` for non-super types.
    function rootClaimByChainId(uint256) external pure returns (Claim rootClaim_) {
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

    /// @inheritdoc IMultiProofGame
    function domain() external view returns (ProofLib.Domain memory) {
        return ProofLib.Domain({
            chainId: DOMAIN_CHAIN_ID,
            proofSystemVersion: DOMAIN_PROOF_SYSTEM_VERSION,
            rollupConfigHash: DOMAIN_ROLLUP_CONFIG_HASH,
            blockInterval: DOMAIN_BLOCK_INTERVAL
        });
    }

    /// @inheritdoc IMultiProofGame
    function l2BlockNumber() external pure returns (uint256) {
        return l2SequenceNumber();
    }

    /// @inheritdoc IMultiProofGame
    function l1OriginHash() external pure returns (bytes32) {
        return Hash.unwrap(l1Head());
    }

    /// @inheritdoc IMultiProofGame
    function l1OriginNumber() external view returns (uint256) {
        return _l1OriginNumber;
    }

    /// @inheritdoc IMultiProofGame
    function state() public view returns (ProofLib.RootState) {
        if (status == GameStatus.DEFENDER_WINS) return ProofLib.RootState.FINALIZED;
        if (status == GameStatus.CHALLENGER_WINS) return ProofLib.RootState.INVALIDATED;
        return challenger == address(0) ? ProofLib.RootState.PROPOSED : ProofLib.RootState.CHALLENGED;
    }

    /// @inheritdoc IMultiProofGame
    function finalizedAt() external view returns (uint64) {
        return status == GameStatus.DEFENDER_WINS ? resolvedAt.raw() : 0;
    }

    /// @inheritdoc IMultiProofGame
    function invalidatedAt() external view returns (uint64) {
        return status == GameStatus.CHALLENGER_WINS ? resolvedAt.raw() : 0;
    }

    /// @inheritdoc IMultiProofGame
    function proofCount() external view returns (uint8) {
        return ProofLib.proofCount(proofBitmap);
    }

    ////////////////////////////////////////////////////////////////
    //                     Initialization                         //
    ////////////////////////////////////////////////////////////////

    /// @notice Initializes a WIP-1006 clone and validates its domain, parent, interval, retry,
    ///         and bond invariants. Any revert bubbles through `DisputeGameFactory.create`.
    function initialize() external payable {
        if (initialized) revert AlreadyInitialized();

        // Only the configured factory may initialize; this also rules out direct initialization
        // of the implementation contract itself.
        if (msg.sender != address(disputeGameFactory)) revert NotDisputeGameFactory(msg.sender);

        // Defense-in-depth against `setInitBond` drifting from the configured proposer bond.
        if (msg.value != proposerBond) revert IncorrectBondAmount();

        bytes memory proposalExtraData = extraData();
        if (proposalExtraData.length < 0x140) revert BadExtraData();
        (
            bytes32 proposalDomainHash_,
            uint256 l2BlockNumber_,
            address parentRef_,
            uint256 attempt_,
            address retryOf_,
            bytes32 l1OriginHash_,
            uint256 l1OriginNumber_,
            bytes memory creationProof_
        ) = abi.decode(proposalExtraData, (bytes32, uint256, address, uint256, address, bytes32, uint256, bytes));
        if (
            creationProof_.length == 0
                || keccak256(proposalExtraData)
                    != keccak256(
                        abi.encode(
                            proposalDomainHash_,
                            l2BlockNumber_,
                            parentRef_,
                            attempt_,
                            retryOf_,
                            l1OriginHash_,
                            l1OriginNumber_,
                            creationProof_
                        )
                    )
        ) {
            revert BadExtraData();
        }

        // Preserves the former propose-time registry pause gate.
        if (anchorStateRegistry.paused()) revert GamePaused();
        if (proposalDomainHash_ != domainHash) revert InvalidDomainHash(domainHash, proposalDomainHash_);

        (Hash anchorRoot, uint256 anchorL2BlockNumber) = anchorStateRegistry.getAnchorRoot();

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
            if (parentType.raw() != GameTypes.MULTI_PROOF_GAME_TYPE.raw()) revert UnexpectedGameType();
            if (parent.status() == GameStatus.CHALLENGER_WINS) revert InvalidParentGame();
            if (anchorStateRegistry.isGameBlacklisted(parent) || anchorStateRegistry.isGameRetired(parent)) {
                revert InvalidParentGame();
            }
            if (!parent.wasRespectedGameTypeWhenCreated()) revert InvalidParentGame();
            // Guards against chaining onto games from an older implementation with a different
            // domain (e.g. after a proof-system version bump reusing the same game type).
            if (IMultiProofGame(address(parent)).domainHash() != domainHash) revert InvalidParentGame();

            startingRootClaim = Claim.unwrap(parent.rootClaim());
            startingL2BlockNumber = parent.l2SequenceNumber();

            // A parent at or below the anchor is stale: proposals extending the anchor state
            // must use the anchor sentinel instead so their starting root is registry-attested.
            if (startingL2BlockNumber <= anchorL2BlockNumber) revert InvalidParentGame();
        }

        uint256 expectedL2BlockNumber = startingL2BlockNumber + DOMAIN_BLOCK_INTERVAL;
        if (l2BlockNumber_ != expectedL2BlockNumber) {
            revert InvalidL2BlockNumber(expectedL2BlockNumber, l2BlockNumber_);
        }
        // Per spec, the sequence number must fit within a uint64.
        if (l2BlockNumber_ > type(uint64).max) revert UnexpectedRootClaim(rootClaim());

        // Retries explicitly reference the previous concrete game because its selected L1 head
        // and creation proof make its factory UUID impossible to reconstruct from the transition.
        if (attempt_ == 0) {
            if (retryOf_ != address(0)) revert BadExtraData();
        } else {
            if (retryOf_.code.length == 0) revert GameNotRetryable(keccak256(abi.encode(retryOf_)));
            IMultiProofGame previous = IMultiProofGame(retryOf_);
            (GameType previousType, Claim previousClaim, bytes memory previousExtraData) = previous.gameData();
            (IDisputeGame registeredPrevious,) =
                disputeGameFactory.games(previousType, previousClaim, previousExtraData);
            if (
                address(registeredPrevious) != retryOf_ || previousType.raw() != GameTypes.MULTI_PROOF_GAME_TYPE.raw()
                    || previous.proposalDomainHash() != domainHash || previous.parentRef() != parentRef_
                    || Claim.unwrap(previousClaim) != Claim.unwrap(rootClaim())
                    || previous.l2SequenceNumber() != l2BlockNumber_ || previous.attempt() != attempt_ - 1
                    || (previous.wasRespectedGameTypeWhenCreated()
                        && (previous.status() != GameStatus.CHALLENGER_WINS
                            || previous.invalidationReason() != ProofLib.InvalidationReason.PROOF_TIMEOUT))
            ) {
                revert GameNotRetryable(keccak256(abi.encode(retryOf_)));
            }
        }

        if (
            l1OriginNumber_ > type(uint64).max || l1OriginHash_ == bytes32(0)
                || _historicalBlockHash(l1OriginNumber_) != l1OriginHash_
        ) {
            revert InvalidL1Head(l1OriginHash_, l1OriginNumber_);
        }
        _l1OriginNumber = uint64(l1OriginNumber_);
        rootId = ProofLib.rootId(
            domainHash, parentRef_, Claim.unwrap(rootClaim()), l2BlockNumber_, l1OriginHash_, _l1OriginNumber
        );
        if (!teeVerifier.verify(rootId, creationProof_)) {
            revert InvalidProof(ProofLib.ProofLane.TEE_ATTESTATION, rootId);
        }
        proofBitmap = ProofLib.laneMask(ProofLib.ProofLane.TEE_ATTESTATION);

        challengeDeadline = uint64(block.timestamp + challengePeriod);
        proofDeadline = uint64(block.timestamp + proofPeriod);
        createdAt = Timestamp.wrap(uint64(block.timestamp));
        initialized = true;

        // Custody the proposer bond in DelayedWETH and track the refund-mode credit.
        refundModeCredit[gameCreator()] += msg.value;
        totalBonds += msg.value;
        weth.deposit{value: msg.value}();

        wasRespectedGameTypeWhenCreated =
            anchorStateRegistry.respectedGameType().raw() == GameTypes.MULTI_PROOF_GAME_TYPE.raw();

        emit WorldChainGameCreated(
            rootId,
            parentRef_,
            Claim.unwrap(rootClaim()),
            l2BlockNumber_,
            l1OriginHash_,
            _l1OriginNumber,
            attempt_,
            gameCreator()
        );
        emit ProofLaneSupported(ProofLib.ProofLane.TEE_ATTESTATION, rootId, proofBitmap);
        if (ProofLib.hasThreshold(proofBitmap, PROOF_THRESHOLD)) {
            emit ProofThresholdReached(rootId, proofBitmap);
        }
    }

    /// @dev `BLOCKHASH` retains 256 blocks; EIP-2935 extends contract access to 8,191.
    function _historicalBlockHash(uint256 blockNumber_) internal view returns (bytes32 hash_) {
        if (blockNumber_ >= block.number) return bytes32(0);
        if (block.number - blockNumber_ <= 256) return blockhash(blockNumber_);

        (bool success, bytes memory result) = HISTORY_STORAGE.staticcall(abi.encode(blockNumber_));
        if (success && result.length == 32) hash_ = abi.decode(result, (bytes32));
    }

    ////////////////////////////////////////////////////////////////
    //                    Challenge and proofs                    //
    ////////////////////////////////////////////////////////////////

    /// @inheritdoc IMultiProofGame
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

    /// @inheritdoc IMultiProofGame
    function submitProofLane(uint8 laneId, bytes calldata proof) external {
        if (status != GameStatus.IN_PROGRESS || challenger == address(0)) {
            revert ClaimAlreadyResolved();
        }
        if (block.timestamp >= proofDeadline) {
            revert ProofPeriodElapsed(block.timestamp, proofDeadline);
        }
        if (laneId >= PROOF_LANE_COUNT) revert InvalidLane(laneId);

        ProofLib.ProofLane lane = ProofLib.ProofLane(laneId);
        uint8 mask = ProofLib.laneMask(lane);
        if ((proofBitmap & mask) != 0) {
            emit DuplicateProofLane(lane, rootId, proofBitmap);
            return;
        }

        if (!_verifierFor(lane).verify(rootId, proof)) {
            revert InvalidProof(lane, rootId);
        }

        bool thresholdAlreadyReached = ProofLib.hasThreshold(proofBitmap, PROOF_THRESHOLD);
        proofBitmap |= mask;
        emit ProofLaneSupported(lane, rootId, proofBitmap);

        // Emit only on the transition to settlement-ready so offchain consumers receive a single signal.
        if (!thresholdAlreadyReached && ProofLib.hasThreshold(proofBitmap, PROOF_THRESHOLD)) {
            emit ProofThresholdReached(rootId, proofBitmap);
        }
    }

    ////////////////////////////////////////////////////////////////
    //                        Resolution                          //
    ////////////////////////////////////////////////////////////////

    /// @inheritdoc IMultiProofGame
    function resolutionStatus()
        external
        view
        returns (bool resolvable, ProofLib.RootState outcome, ProofLib.InvalidationReason reason)
    {
        if (status != GameStatus.IN_PROGRESS) {
            return (false, state(), invalidationReason);
        }

        (GameStatus parentStatus, bool parentBlacklisted) = _parentResolution();
        if (parentBlacklisted || parentStatus == GameStatus.CHALLENGER_WINS) {
            return (true, ProofLib.RootState.INVALIDATED, ProofLib.InvalidationReason.INVALID_PARENT);
        }
        if (parentStatus == GameStatus.IN_PROGRESS) {
            return (false, state(), ProofLib.InvalidationReason.NONE);
        }

        if (challenger == address(0)) {
            return block.timestamp >= challengeDeadline
                ? (true, ProofLib.RootState.FINALIZED, ProofLib.InvalidationReason.NONE)
                : (false, ProofLib.RootState.PROPOSED, ProofLib.InvalidationReason.NONE);
        }

        if (ProofLib.hasThreshold(proofBitmap, PROOF_THRESHOLD)) {
            return (true, ProofLib.RootState.FINALIZED, ProofLib.InvalidationReason.NONE);
        }
        return block.timestamp >= proofDeadline
            ? (true, ProofLib.RootState.INVALIDATED, ProofLib.InvalidationReason.PROOF_TIMEOUT)
            : (false, ProofLib.RootState.CHALLENGED, ProofLib.InvalidationReason.NONE);
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
            invalidationReason = ProofLib.InvalidationReason.INVALID_PARENT;
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
        } else if (ProofLib.hasThreshold(proofBitmap, PROOF_THRESHOLD)) {
            // A challenged game finalizes as soon as enough independent proof lanes support it.
            // The proposer takes the challenger bond.
            status = GameStatus.DEFENDER_WINS;
            normalModeCredit[gameCreator()] = totalBonds;
        } else if (block.timestamp >= proofDeadline) {
            // A challenged game below threshold times out once its proof window expires. The
            // challenger takes the proposer bond.
            status = GameStatus.CHALLENGER_WINS;
            invalidationReason = ProofLib.InvalidationReason.PROOF_TIMEOUT;
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

    /// @inheritdoc IMultiProofGame
    /// @dev Locks in `REFUND` for improper games — blacklisted, retired, or otherwise
    ///      invalidated by the registry. Must not revert once closed, or `claimCredit` breaks.
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

    /// @inheritdoc IMultiProofGame
    /// @dev Two-phase: the first call unlocks in DelayedWETH, the second (after the WETH
    ///      delay) withdraws and transfers.
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

    /// @inheritdoc IMultiProofGame
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

    function _verifierFor(ProofLib.ProofLane lane) internal view returns (IWorldChainProofVerifier) {
        if (lane == ProofLib.ProofLane.VALIDITY_PROOF) return validityProofVerifier;
        if (lane == ProofLib.ProofLane.TEE_ATTESTATION) return teeVerifier;
        return securityCouncil;
    }
}
