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
    Timestamp,
    Proposal
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
///         at a fixed block interval. An unchallenged proposal requires one valid proof lane;
///         a challenged proposal requires enough independent lanes (validity proof, TEE
///         attestation, security council) to reach the configured threshold. Bond custody uses
///         `DelayedWETH` with the two-phase unlock/withdraw claim flow.
/// @dev Structure follows `ZKDisputeGame`; challenge/lane semantics are World Chain specific.
contract MultiProofGame is Clone, ISemver, IMultiProofGame {
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
    address public immutable proofTimeoutRecipient;

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
                || config.proofTimeoutRecipient == address(0) || address(config.disputeGameFactory) == address(0)
                || address(config.anchorStateRegistry) == address(0) || address(config.weth) == address(0)
                || address(config.stakingRegistry) == address(0) || address(config.validityProofVerifier) == address(0)
                || address(config.teeVerifier) == address(0) || address(config.securityCouncil) == address(0)
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
        proofTimeoutRecipient = config.proofTimeoutRecipient;
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

    function extraData() public pure returns (bytes memory extraData_) {
        extraData_ = _getArgBytes(0x54, 0x80);
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

        address parentRef_ = parentRef();

        if (parentRef_ == address(anchorStateRegistry)) {
            Proposal memory startingOutputRoot = anchorStateRegistry.getStartingAnchorRoot();
            startingRootClaim = startingOutputRoot.root.raw();
            startingL2BlockNumber = startingOutputRoot.l2SequenceNumber;
        } else {
            IDisputeGame parent = IDisputeGame(parentRef_);

            if (!_isValidGame(parent)) {
                revert InvalidParentGame();
            }

            startingRootClaim = Claim.unwrap(parent.rootClaim());
            startingL2BlockNumber = parent.l2SequenceNumber();
        }

        uint256 expectedL2BlockNumber = startingL2BlockNumber + DOMAIN_BLOCK_INTERVAL;
        if (l2SequenceNumber() != expectedL2BlockNumber) {
            revert InvalidL2BlockNumber(expectedL2BlockNumber, l2SequenceNumber());
        }
        // Per spec, the sequence number must fit within a uint64.
        if (l2SequenceNumber() > type(uint64).max) revert UnexpectedRootClaim(rootClaim());

        // Retries: attempt N is only proposable when attempt N-1 for the identical transition
        // timed out on proofs or was created before WIP-1006 became respected. The latter
        // prevents a pre-cutover game from permanently occupying the factory UUID. Inherited
        // invalidations rebase onto a replacement parent and therefore restart at attempt zero.
        // Duplicate attempts are impossible: the factory UUID covers (gameType, rootClaim,
        // extraData).
        if (attempt() > 0) {
            bytes memory previousExtraData = abi.encode(domainHash, l2SequenceNumber(), parentRef_, attempt() - 1);
            (IDisputeGame previous,) =
                disputeGameFactory.games(GameTypes.MULTI_PROOF_GAME_TYPE, rootClaim(), previousExtraData);
            if (
                address(previous) == address(0)
                    || (previous.wasRespectedGameTypeWhenCreated()
                        && (previous.status() != GameStatus.CHALLENGER_WINS
                            || IMultiProofGame(address(previous)).invalidationReason()
                                != ProofLib.InvalidationReason.PROOF_TIMEOUT))
            ) {
                revert GameNotRetryable(keccak256(
                        abi.encode(GameTypes.MULTI_PROOF_GAME_TYPE, rootClaim(), previousExtraData)
                    ));
            }
        }

        // `initialize` runs in the same transaction as `DisputeGameFactory.create`, which set
        // `l1Head = blockhash(block.number - 1)`.
        _l1OriginNumber = uint64(block.number - 1);
        rootId = ProofLib.rootId(
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
            anchorStateRegistry.respectedGameType().raw() == GameTypes.MULTI_PROOF_GAME_TYPE.raw();

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

    /// @notice Checks if the game is registered, respected, not blacklisted, not retired, and not challenged.
    /// @param game The game to check.
    function _isValidGame(IDisputeGame game) internal view returns (bool) {
        return anchorStateRegistry.isGameRegistered(game) && anchorStateRegistry.isGameRespected(game)
            && !anchorStateRegistry.isGameBlacklisted(game) && !anchorStateRegistry.isGameRetired(game)
            && (game.status() != GameStatus.CHALLENGER_WINS);
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
        if (status != GameStatus.IN_PROGRESS) revert ClaimAlreadyResolved();

        uint64 deadline = challenger == address(0) ? challengeDeadline : proofDeadline;
        if (block.timestamp >= deadline) {
            revert ProofPeriodElapsed(block.timestamp, deadline);
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

        // TODO: Decide whether reaching PROOF_THRESHOLD should make an unchallenged game
        // immediately resolvable too. The current WIP guarantees the full challenge window even
        // with threshold support, while challenged games resolve as soon as they reach the same
        // threshold; update the WIP together with this branch if threshold support is intended
        // to provide fast finality in both states.
        if (challenger == address(0)) {
            if (block.timestamp < challengeDeadline) {
                return (false, ProofLib.RootState.PROPOSED, ProofLib.InvalidationReason.NONE);
            }
            return proofBitmap != 0
                ? (true, ProofLib.RootState.FINALIZED, ProofLib.InvalidationReason.NONE)
                : (true, ProofLib.RootState.INVALIDATED, ProofLib.InvalidationReason.PROOF_TIMEOUT);
        }

        if (ProofLib.hasThreshold(proofBitmap, PROOF_THRESHOLD)) {
            return (true, ProofLib.RootState.FINALIZED, ProofLib.InvalidationReason.NONE);
        }
        return block.timestamp >= proofDeadline
            ? (true, ProofLib.RootState.INVALIDATED, ProofLib.InvalidationReason.PROOF_TIMEOUT)
            : (false, ProofLib.RootState.CHALLENGED, ProofLib.InvalidationReason.NONE);
    }

    /// @notice Resolves the game.
    ///         `DEFENDER_WINS` when a proven game expires unchallenged, or when enough proof
    ///         lanes support a challenged claim. `CHALLENGER_WINS` when an applicable proof
    ///         window expires below its requirement, or when the parent game is invalid.
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
        } else if (challenger == address(0) && proofBitmap != 0) {
            // Any configured proof lane may support the optimistic path. The offchain defender
            // uses TEE attestations by policy, but the protocol does not privilege a lane.
            if (block.timestamp < challengeDeadline) revert GameNotOver();
            status = GameStatus.DEFENDER_WINS;
            normalModeCredit[gameCreator()] = totalBonds;
        } else if (challenger == address(0)) {
            // A proofless proposal cannot win merely because nobody challenged it.
            if (block.timestamp < challengeDeadline) revert GameNotOver();
            status = GameStatus.CHALLENGER_WINS;
            invalidationReason = ProofLib.InvalidationReason.PROOF_TIMEOUT;
            normalModeCredit[proofTimeoutRecipient] = totalBonds;
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
