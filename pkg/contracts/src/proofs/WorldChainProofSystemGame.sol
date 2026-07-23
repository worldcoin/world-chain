// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {WorldChainProofLib} from "./WorldChainProofLib.sol";
import {Claim, GameStatus, GameType, Hash, Timestamp, GameTypes} from "./DisputeTypes.sol";
import {IWorldChainAnchorStateRegistry} from "./interfaces/IWorldChainAnchorStateRegistry.sol";
import {IWorldChainProofVerifier} from "./interfaces/IWorldChainProofVerifier.sol";
import {IWorldChainProofSystemGame} from "./interfaces/IWorldChainProofSystemGame.sol";
import {IWorldChainStakingRegistry} from "./interfaces/IWorldChainStakingRegistry.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";

contract WorldChainProofSystemGame is ReentrancyGuardTransient, IWorldChainProofSystemGame {
    using WorldChainProofLib for uint8;

    struct ProposalInit {
        address factory;
        address anchorStateRegistry;
        uint256 attempt;
        address proposer;
        address parentRef;
        bytes32 startingRootClaim;
        uint256 startingL2BlockNumber;
        bytes32 rootClaim;
        uint256 l2BlockNumber;
        bytes32 l1OriginHash;
        uint256 l1OriginNumber;
    }

    struct ActivationConfig {
        bytes32 domainHash;
        uint64 challengePeriod;
        uint64 proofPeriod;
        uint256 proposerBond;
        uint256 challengerBond;
        uint8 proofThreshold;
        IWorldChainProofVerifier validityProofVerifier;
        IWorldChainProofVerifier teeVerifier;
        IWorldChainProofVerifier securityCouncil;
        IWorldChainStakingRegistry stakingRegistry;
    }

    enum ResolutionStatus {
        NOT_READY,
        RESOLVABLE,
        PARENT_NOT_RESOLVED,
        ALREADY_RESOLVED
    }

    struct ResolutionEvaluation {
        ResolutionStatus status;
        WorldChainProofLib.RootState outcome;
        WorldChainProofLib.InvalidationReason reason;
        WorldChainProofLib.RootState parentState;
    }

    error InvalidBond(uint256 expected, uint256 actual);
    error InvalidState(WorldChainProofLib.RootState expected, WorldChainProofLib.RootState actual);
    error ChallengePeriodElapsed(uint256 timestamp, uint256 challengeDeadline);
    error ProofPeriodElapsed(uint256 timestamp, uint256 proofDeadline);
    error NotReady();
    error ParentGameNotResolved(address parent, WorldChainProofLib.RootState state);
    error AlreadyResolved(WorldChainProofLib.RootState state);
    error UnstakedChallenger(address challenger);
    error DuplicateChallenge(address challenger);
    error InvalidLane(uint8 lane);
    error InvalidProof(WorldChainProofLib.ProofLane lane, bytes32 rootId);
    error NoClaim(address recipient);
    error TransferFailed(address recipient, uint256 amount);

    event Challenged(address indexed challenger, uint64 proofDeadline);
    event ProofLaneSupported(WorldChainProofLib.ProofLane indexed lane, bytes32 indexed rootId, uint8 proofBitmap);
    event ProofThresholdReached(bytes32 indexed rootId, uint8 proofBitmap);
    event DuplicateProofLane(WorldChainProofLib.ProofLane indexed lane, bytes32 indexed rootId, uint8 proofBitmap);
    event Finalized(bytes32 indexed rootId);
    event Invalidated(bytes32 indexed rootId, WorldChainProofLib.InvalidationReason reason);
    event Withdrawn(address indexed recipient, uint256 amount);

    /// Number of distinct proof lanes required to finalize a challenged root.
    /// Set per-deployment by the factory (defaults to `WorldChainProofLib.PROOF_THRESHOLD`).
    uint8 public immutable PROOF_THRESHOLD;
    uint8 public constant PROOF_LANE_COUNT = WorldChainProofLib.PROOF_LANE_COUNT;

    address public immutable override factory;
    address public immutable override anchorStateRegistry;
    uint256 public immutable override attempt;
    address public immutable override gameCreator;
    address public immutable override parentRef;
    bytes32 public immutable override startingRootClaim;
    uint256 public immutable override startingL2BlockNumber;
    bytes32 public immutable override rootClaim;
    uint256 public immutable override l2SequenceNumber;
    bytes32 private immutable _l1Head;
    uint256 public immutable override l1OriginNumber;
    bytes32 public immutable override domainHash;
    bytes32 public immutable override rootId;

    uint64 public immutable challengePeriod;
    uint64 public immutable proofPeriod;
    uint256 public immutable proposerBond;
    uint256 public immutable challengerBond;

    IWorldChainProofVerifier public immutable validityProofVerifier;
    IWorldChainProofVerifier public immutable teeVerifier;
    IWorldChainProofVerifier public immutable securityCouncil;
    IWorldChainStakingRegistry public immutable stakingRegistry;

    uint64 public override createdAt;
    uint64 public override challengeDeadline;
    uint64 public override proofDeadline;
    uint64 private _resolvedAt;
    uint8 public override proofBitmap;
    WorldChainProofLib.RootState public override state;
    WorldChainProofLib.InvalidationReason public override invalidationReason;
    bool public override wasRespectedGameTypeWhenCreated;

    address payable public challenger;
    uint256 public postedChallengerBond;
    mapping(address recipient => uint256 amount) internal payoutCredits;

    constructor(ProposalInit memory proposal, ActivationConfig memory config) payable {
        if (msg.value != config.proposerBond) revert InvalidBond(config.proposerBond, msg.value);

        factory = proposal.factory;
        anchorStateRegistry = proposal.anchorStateRegistry;
        attempt = proposal.attempt;
        gameCreator = proposal.proposer;
        parentRef = proposal.parentRef;
        startingRootClaim = proposal.startingRootClaim;
        startingL2BlockNumber = proposal.startingL2BlockNumber;
        rootClaim = proposal.rootClaim;
        l2SequenceNumber = proposal.l2BlockNumber;
        _l1Head = proposal.l1OriginHash;
        l1OriginNumber = proposal.l1OriginNumber;
        domainHash = config.domainHash;
        rootId = WorldChainProofLib.rootId(
            config.domainHash,
            proposal.parentRef,
            proposal.rootClaim,
            proposal.l2BlockNumber,
            proposal.l1OriginHash,
            proposal.l1OriginNumber
        );
        challengePeriod = config.challengePeriod;
        proofPeriod = config.proofPeriod;
        proposerBond = config.proposerBond;
        challengerBond = config.challengerBond;
        PROOF_THRESHOLD = config.proofThreshold;
        validityProofVerifier = config.validityProofVerifier;
        teeVerifier = config.teeVerifier;
        securityCouncil = config.securityCouncil;
        stakingRegistry = config.stakingRegistry;

        createdAt = uint64(block.timestamp);
        challengeDeadline = uint64(block.timestamp + config.challengePeriod);
        state = WorldChainProofLib.RootState.PROPOSED;
        wasRespectedGameTypeWhenCreated = GameType.unwrap(
            IWorldChainAnchorStateRegistry(proposal.anchorStateRegistry).respectedGameType()
        ) == GameType.unwrap(GameTypes.WIP_1006);
    }

    receive() external payable {}

    /// @notice Translates the WIP-1006 lifecycle state into the outcome read by the Portal and ASR.
    function status() external view override returns (GameStatus) {
        WorldChainProofLib.RootState currentState = state;
        if (currentState == WorldChainProofLib.RootState.FINALIZED) return GameStatus.DEFENDER_WINS;
        if (currentState == WorldChainProofLib.RootState.INVALIDATED) return GameStatus.CHALLENGER_WINS;
        return GameStatus.IN_PROGRESS;
    }

    function resolvedAt() external view override returns (Timestamp) {
        return Timestamp.wrap(_resolvedAt);
    }

    function gameType() external pure override returns (GameType) {
        return GameTypes.WIP_1006;
    }

    function l1Head() external view override returns (Hash) {
        return Hash.wrap(_l1Head);
    }

    function extraData() public view override returns (bytes memory) {
        return abi.encode(l2SequenceNumber, parentRef);
    }

    function gameData() external view override returns (GameType, Claim, bytes memory) {
        return (GameTypes.WIP_1006, Claim.wrap(rootClaim), extraData());
    }

    function proofCount() external view returns (uint8) {
        return WorldChainProofLib.proofCount(proofBitmap);
    }

    function challenge() external payable {
        if (state != WorldChainProofLib.RootState.PROPOSED && state != WorldChainProofLib.RootState.CHALLENGED) {
            revert InvalidState(WorldChainProofLib.RootState.PROPOSED, state);
        }
        if (block.timestamp >= challengeDeadline) {
            revert ChallengePeriodElapsed(block.timestamp, challengeDeadline);
        }
        if (!stakingRegistry.isStaked(msg.sender)) revert UnstakedChallenger(msg.sender);
        if (msg.value != challengerBond) revert InvalidBond(challengerBond, msg.value);
        if (challenger != address(0)) revert DuplicateChallenge(challenger);

        challenger = payable(msg.sender);
        postedChallengerBond = msg.value;

        if (state == WorldChainProofLib.RootState.PROPOSED) {
            state = WorldChainProofLib.RootState.CHALLENGED;
            proofDeadline = uint64(block.timestamp + proofPeriod);
        }

        emit Challenged(msg.sender, proofDeadline);
    }

    /// @notice Returns the ETH amount `recipient` can withdraw from this game.
    function claimable(address recipient) external view override returns (uint256) {
        return _claimable(recipient);
    }

    /// @notice Permissionlessly withdraws `recipient`'s claim to `recipient`.
    /// @dev The challenger, defender/prover-service automation, or keepers can call this after resolution;
    ///      the caller cannot redirect funds away from `recipient`.
    function withdraw(address payable recipient) external override nonReentrant {
        address account = recipient;
        uint256 amount = _claimable(account);
        if (amount == 0) revert NoClaim(account);

        uint256 refundablePrincipal = _refundableChallengerPrincipal(account);
        payoutCredits[account] = 0;
        if (refundablePrincipal != 0) postedChallengerBond = 0;

        _transfer(recipient, amount);
        emit Withdrawn(account, amount);
    }

    function submitProofLane(uint8 laneId, bytes calldata proof) external {
        if (state != WorldChainProofLib.RootState.CHALLENGED) {
            revert InvalidState(WorldChainProofLib.RootState.CHALLENGED, state);
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

    /// @notice Returns whether this game can resolve now and the resulting state and invalidation reason.
    function resolutionStatus()
        external
        view
        override
        returns (bool resolvable, WorldChainProofLib.RootState outcome, WorldChainProofLib.InvalidationReason reason)
    {
        ResolutionEvaluation memory evaluation = _evaluateResolution();
        return (evaluation.status == ResolutionStatus.RESOLVABLE, evaluation.outcome, evaluation.reason);
    }

    /// @notice Settles this game after evaluating its blacklist, parent, deadline, and proof-threshold conditions.
    /// @dev Resolution assigns pull-based bond claims; call `withdraw(recipient)` to transfer claimable ETH.
    function resolve()
        external
        override
        returns (WorldChainProofLib.RootState outcome, WorldChainProofLib.InvalidationReason reason)
    {
        ResolutionEvaluation memory evaluation = _evaluateResolution();
        if (evaluation.status != ResolutionStatus.RESOLVABLE) {
            if (evaluation.status == ResolutionStatus.PARENT_NOT_RESOLVED) {
                revert ParentGameNotResolved(parentRef, evaluation.parentState);
            }
            if (evaluation.status == ResolutionStatus.ALREADY_RESOLVED) revert AlreadyResolved(state);
            revert NotReady();
        }

        if (evaluation.outcome == WorldChainProofLib.RootState.FINALIZED) {
            _finalize();
        } else {
            _invalidate(evaluation.reason);
        }

        return (evaluation.outcome, evaluation.reason);
    }

    /// @notice Asks the registry to advance its accepted anchor to this game.
    /// @dev This is separate from Portal withdrawal finalization, which reads `isGameClaimValid` directly.
    ///      TODO: Before Portal activation, confirm whether anchor-update failures should remain
    ///      visible or follow Base's best-effort close behavior after eligibility checks.
    function closeGame() external override {
        IWorldChainAnchorStateRegistry(anchorStateRegistry).setAnchorState(address(this));
    }

    function _evaluateResolution() internal view returns (ResolutionEvaluation memory evaluation) {
        WorldChainProofLib.RootState currentState = state;
        if (
            currentState == WorldChainProofLib.RootState.FINALIZED
                || currentState == WorldChainProofLib.RootState.INVALIDATED
        ) {
            evaluation.outcome = currentState;
            evaluation.reason = invalidationReason;
            evaluation.status = ResolutionStatus.ALREADY_RESOLVED;
            return evaluation;
        }

        IWorldChainAnchorStateRegistry registry = IWorldChainAnchorStateRegistry(anchorStateRegistry);
        // 1. A governance blacklist invalidates this game before parent or deadline evaluation.
        if (registry.isGameBlacklisted(address(this))) {
            evaluation.status = ResolutionStatus.RESOLVABLE;
            evaluation.outcome = WorldChainProofLib.RootState.INVALIDATED;
            evaluation.reason = WorldChainProofLib.InvalidationReason.BLACKLISTED;
            return evaluation;
        }

        WorldChainProofLib.RootState parentState;
        bool parentBlacklisted;
        if (parentRef == anchorStateRegistry) {
            // The registry sentinel represents the immutable deployment anchor before a game anchor exists.
            parentState = WorldChainProofLib.RootState.FINALIZED;
        } else {
            parentBlacklisted = registry.isGameBlacklisted(parentRef);
            if (!parentBlacklisted) parentState = IWorldChainProofSystemGame(parentRef).state();
        }

        if (
            parentState == WorldChainProofLib.RootState.PROPOSED
                || parentState == WorldChainProofLib.RootState.CHALLENGED
        ) {
            // 2. A proposed or challenged parent must resolve before its descendant.
            evaluation.outcome = currentState;
            evaluation.status = ResolutionStatus.PARENT_NOT_RESOLVED;
            evaluation.parentState = parentState;
            return evaluation;
        }
        if (
            parentBlacklisted || parentState == WorldChainProofLib.RootState.INVALIDATED
                || parentState == WorldChainProofLib.RootState.NONE
        ) {
            // 3. A blacklisted, invalidated, or unset parent invalidates its descendant.
            evaluation.status = ResolutionStatus.RESOLVABLE;
            evaluation.outcome = WorldChainProofLib.RootState.INVALIDATED;
            evaluation.reason = WorldChainProofLib.InvalidationReason.INVALID_PARENT;
            return evaluation;
        }

        if (currentState == WorldChainProofLib.RootState.PROPOSED) {
            if (block.timestamp < challengeDeadline) {
                // 4. An unchallenged proposal cannot finalize while its challenge window is active.
                evaluation.outcome = currentState;
                return evaluation;
            }
            // 5. An unchallenged proposal finalizes after its challenge window expires.
            // Safety therefore relies on every incorrect claim being challenged before this deadline.
            evaluation.status = ResolutionStatus.RESOLVABLE;
            evaluation.outcome = WorldChainProofLib.RootState.FINALIZED;
            return evaluation;
        }

        // 6. A challenged game finalizes as soon as enough independent proof lanes support it.
        if (WorldChainProofLib.hasThreshold(proofBitmap, PROOF_THRESHOLD)) {
            evaluation.status = ResolutionStatus.RESOLVABLE;
            evaluation.outcome = WorldChainProofLib.RootState.FINALIZED;
            return evaluation;
        }
        if (block.timestamp < proofDeadline) {
            // 7. A challenged game below threshold waits while its proof window is active.
            evaluation.outcome = currentState;
            return evaluation;
        }

        // 8. A challenged game below threshold times out once its proof window expires.
        evaluation.status = ResolutionStatus.RESOLVABLE;
        evaluation.outcome = WorldChainProofLib.RootState.INVALIDATED;
        evaluation.reason = WorldChainProofLib.InvalidationReason.PROOF_TIMEOUT;
    }

    function _invalidate(WorldChainProofLib.InvalidationReason reason) internal {
        state = WorldChainProofLib.RootState.INVALIDATED;
        invalidationReason = reason;
        _resolvedAt = uint64(block.timestamp);

        uint256 balance = address(this).balance;
        if (reason == WorldChainProofLib.InvalidationReason.PROOF_TIMEOUT) {
            payoutCredits[challenger] += balance;
            // The full-balance credit already includes the challenger bond, so it is no longer separately refundable.
            postedChallengerBond = 0;
        } else {
            uint256 proposerRefund = balance - postedChallengerBond;
            if (proposerRefund != 0) payoutCredits[gameCreator] += proposerRefund;
        }

        emit Invalidated(rootId, reason);
    }

    function _finalize() internal {
        state = WorldChainProofLib.RootState.FINALIZED;
        _resolvedAt = uint64(block.timestamp);

        uint256 payout = address(this).balance;
        if (payout != 0) {
            payoutCredits[gameCreator] += payout;
        }

        emit Finalized(rootId);
    }

    function _verifierFor(WorldChainProofLib.ProofLane lane) internal view returns (IWorldChainProofVerifier) {
        if (lane == WorldChainProofLib.ProofLane.VALIDITY_PROOF) return validityProofVerifier;
        if (lane == WorldChainProofLib.ProofLane.TEE_ATTESTATION) return teeVerifier;
        return securityCouncil;
    }

    function _claimable(address recipient) internal view returns (uint256) {
        return payoutCredits[recipient] + _refundableChallengerPrincipal(recipient);
    }

    /// @dev Proof timeout credits the full balance to the challenger, so only inherited or governance invalidations
    ///      refund the challenger bond separately.
    function _refundableChallengerPrincipal(address recipient) internal view returns (uint256) {
        bool challengerBondIsRefundable = state == WorldChainProofLib.RootState.INVALIDATED && recipient == challenger
            && (invalidationReason == WorldChainProofLib.InvalidationReason.INVALID_PARENT
                || invalidationReason == WorldChainProofLib.InvalidationReason.BLACKLISTED);

        return challengerBondIsRefundable ? postedChallengerBond : 0;
    }

    function _transfer(address payable recipient, uint256 amount) internal {
        if (amount == 0) return;
        (bool ok,) = recipient.call{value: amount}("");
        if (!ok) revert TransferFailed(recipient, amount);
    }
}
