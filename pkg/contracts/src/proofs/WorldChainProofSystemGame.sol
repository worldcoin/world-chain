// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {WorldChainProofLib} from "./WorldChainProofLib.sol";
import {IWorldChainAnchorStateRegistry} from "./interfaces/IWorldChainAnchorStateRegistry.sol";
import {IWorldChainProofVerifier} from "./interfaces/IWorldChainProofVerifier.sol";
import {IWorldChainProofSystemGame} from "./interfaces/IWorldChainProofSystemGame.sol";
import {IWorldChainStakingRegistry} from "./interfaces/IWorldChainStakingRegistry.sol";

contract WorldChainProofSystemGame {
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

    enum ResolutionBlocker {
        NONE,
        NOT_READY,
        PARENT_NOT_RESOLVED,
        ALREADY_RESOLVED
    }

    struct ResolutionEvaluation {
        bool resolvable;
        WorldChainProofLib.RootState outcome;
        WorldChainProofLib.InvalidationReason reason;
        ResolutionBlocker blocker;
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
    error TransferFailed(address recipient, uint256 amount);

    event Challenged(address indexed challenger, uint64 proofDeadline);
    event ProofLaneSupported(WorldChainProofLib.ProofLane indexed lane, bytes32 indexed rootId, uint8 proofBitmap);
    event ProofThresholdReached(bytes32 indexed rootId, uint8 proofBitmap);
    event DuplicateProofLane(WorldChainProofLib.ProofLane indexed lane, bytes32 indexed rootId, uint8 proofBitmap);
    event Finalized(bytes32 indexed rootId);
    event Invalidated(bytes32 indexed rootId, WorldChainProofLib.InvalidationReason reason);

    /// Number of distinct proof lanes required to finalize a challenged root.
    /// Set per-deployment by the factory (defaults to `WorldChainProofLib.PROOF_THRESHOLD`).
    uint8 public immutable PROOF_THRESHOLD;
    uint8 public constant PROOF_LANE_COUNT = WorldChainProofLib.PROOF_LANE_COUNT;

    address public immutable factory;
    address public immutable anchorStateRegistry;
    uint256 public immutable attempt;
    address payable public immutable proposer;
    address public immutable parentRef;
    bytes32 public immutable startingRootClaim;
    uint256 public immutable startingL2BlockNumber;
    bytes32 public immutable rootClaim;
    uint256 public immutable l2BlockNumber;
    bytes32 public immutable l1OriginHash;
    uint256 public immutable l1OriginNumber;
    bytes32 public immutable domainHash;
    bytes32 public immutable rootId;

    uint64 public immutable challengePeriod;
    uint64 public immutable proofPeriod;
    uint256 public immutable proposerBond;
    uint256 public immutable challengerBond;

    IWorldChainProofVerifier public immutable validityProofVerifier;
    IWorldChainProofVerifier public immutable teeVerifier;
    IWorldChainProofVerifier public immutable securityCouncil;
    IWorldChainStakingRegistry public immutable stakingRegistry;

    uint64 public createdAt;
    uint64 public challengeDeadline;
    uint64 public challengedAt;
    uint64 public proofDeadline;
    uint64 public finalizedAt;
    uint64 public invalidatedAt;
    uint8 public proofBitmap;
    WorldChainProofLib.RootState public state;
    WorldChainProofLib.InvalidationReason public invalidationReason;

    address[] public challengers;
    mapping(address challenger => uint256 amount) public challengerBonds;

    constructor(ProposalInit memory proposal, ActivationConfig memory config) payable {
        if (msg.value != config.proposerBond) revert InvalidBond(config.proposerBond, msg.value);

        factory = proposal.factory;
        anchorStateRegistry = proposal.anchorStateRegistry;
        attempt = proposal.attempt;
        proposer = payable(proposal.proposer);
        parentRef = proposal.parentRef;
        startingRootClaim = proposal.startingRootClaim;
        startingL2BlockNumber = proposal.startingL2BlockNumber;
        rootClaim = proposal.rootClaim;
        l2BlockNumber = proposal.l2BlockNumber;
        l1OriginHash = proposal.l1OriginHash;
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
    }

    receive() external payable {}

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
        if (challengerBonds[msg.sender] != 0) revert DuplicateChallenge(msg.sender);

        challengerBonds[msg.sender] = msg.value;
        challengers.push(msg.sender);

        if (state == WorldChainProofLib.RootState.PROPOSED) {
            state = WorldChainProofLib.RootState.CHALLENGED;
            challengedAt = uint64(block.timestamp);
            proofDeadline = uint64(block.timestamp + proofPeriod);
        }

        emit Challenged(msg.sender, proofDeadline);
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
        returns (bool resolvable, WorldChainProofLib.RootState outcome, WorldChainProofLib.InvalidationReason reason)
    {
        ResolutionEvaluation memory evaluation = _evaluateResolution();
        return (evaluation.resolvable, evaluation.outcome, evaluation.reason);
    }

    /// @notice Settles this game after evaluating its blacklist, parent, deadline, and proof-threshold conditions.
    function resolve()
        external
        returns (WorldChainProofLib.RootState outcome, WorldChainProofLib.InvalidationReason reason)
    {
        ResolutionEvaluation memory evaluation = _evaluateResolution();
        if (!evaluation.resolvable) {
            if (evaluation.blocker == ResolutionBlocker.PARENT_NOT_RESOLVED) {
                revert ParentGameNotResolved(parentRef, evaluation.parentState);
            }
            if (evaluation.blocker == ResolutionBlocker.ALREADY_RESOLVED) revert AlreadyResolved(state);
            revert NotReady();
        }

        if (evaluation.outcome == WorldChainProofLib.RootState.FINALIZED) {
            _finalize();
        } else {
            _invalidate(evaluation.reason);
        }

        return (evaluation.outcome, evaluation.reason);
    }

    function _evaluateResolution() internal view returns (ResolutionEvaluation memory evaluation) {
        WorldChainProofLib.RootState currentState = state;
        if (
            currentState == WorldChainProofLib.RootState.FINALIZED
                || currentState == WorldChainProofLib.RootState.INVALIDATED
        ) {
            evaluation.outcome = currentState;
            evaluation.reason = invalidationReason;
            evaluation.blocker = ResolutionBlocker.ALREADY_RESOLVED;
            return evaluation;
        }

        IWorldChainAnchorStateRegistry registry = IWorldChainAnchorStateRegistry(anchorStateRegistry);
        if (registry.blacklistedGames(address(this))) {
            evaluation.resolvable = true;
            evaluation.outcome = WorldChainProofLib.RootState.INVALIDATED;
            evaluation.reason = WorldChainProofLib.InvalidationReason.BLACKLISTED;
            return evaluation;
        }

        WorldChainProofLib.RootState parentState;
        if (parentRef == anchorStateRegistry) {
            // The registry sentinel represents the accepted anchor, so it is a finalized parent without game state.
            parentState = WorldChainProofLib.RootState.FINALIZED;
        } else if (registry.blacklistedGames(parentRef)) {
            evaluation.resolvable = true;
            evaluation.outcome = WorldChainProofLib.RootState.INVALIDATED;
            evaluation.reason = WorldChainProofLib.InvalidationReason.INVALID_PARENT;
            return evaluation;
        } else {
            parentState = IWorldChainProofSystemGame(parentRef).state();
        }

        if (parentState == WorldChainProofLib.RootState.INVALIDATED || parentState == WorldChainProofLib.RootState.NONE)
        {
            evaluation.resolvable = true;
            evaluation.outcome = WorldChainProofLib.RootState.INVALIDATED;
            evaluation.reason = WorldChainProofLib.InvalidationReason.INVALID_PARENT;
            return evaluation;
        }
        if (parentState != WorldChainProofLib.RootState.FINALIZED) {
            evaluation.outcome = currentState;
            evaluation.blocker = ResolutionBlocker.PARENT_NOT_RESOLVED;
            evaluation.parentState = parentState;
            return evaluation;
        }

        if (currentState == WorldChainProofLib.RootState.PROPOSED) {
            if (block.timestamp < challengeDeadline) {
                evaluation.outcome = currentState;
                evaluation.blocker = ResolutionBlocker.NOT_READY;
                return evaluation;
            }
            evaluation.resolvable = true;
            evaluation.outcome = WorldChainProofLib.RootState.FINALIZED;
            return evaluation;
        }

        if (WorldChainProofLib.hasThreshold(proofBitmap, PROOF_THRESHOLD)) {
            evaluation.resolvable = true;
            evaluation.outcome = WorldChainProofLib.RootState.FINALIZED;
            return evaluation;
        }
        if (block.timestamp < proofDeadline) {
            evaluation.outcome = currentState;
            evaluation.blocker = ResolutionBlocker.NOT_READY;
            return evaluation;
        }

        evaluation.resolvable = true;
        evaluation.outcome = WorldChainProofLib.RootState.INVALIDATED;
        evaluation.reason = WorldChainProofLib.InvalidationReason.PROOF_TIMEOUT;
    }

    function _invalidate(WorldChainProofLib.InvalidationReason reason) internal {
        state = WorldChainProofLib.RootState.INVALIDATED;
        invalidationReason = reason;
        invalidatedAt = uint64(block.timestamp);

        // TODO: Replace temporary push payouts with pull-based credits so recipient callbacks cannot block resolution.
        for (uint256 i = 0; i < challengers.length; i++) {
            address challenger = challengers[i];
            uint256 amount = challengerBonds[challenger];
            challengerBonds[challenger] = 0;
            _transfer(payable(challenger), amount);
        }

        uint256 forfeited = address(this).balance;
        // Only a direct proof timeout is attributable to this proposer; inherited and governance failures refund it.
        if (forfeited != 0 && reason == WorldChainProofLib.InvalidationReason.PROOF_TIMEOUT && challengers.length != 0)
        {
            _transfer(payable(challengers[0]), forfeited);
        } else if (forfeited != 0) {
            _transfer(proposer, forfeited);
        }

        emit Invalidated(rootId, reason);
    }

    function _finalize() internal {
        state = WorldChainProofLib.RootState.FINALIZED;
        finalizedAt = uint64(block.timestamp);

        uint256 payout = address(this).balance;
        if (payout != 0) {
            _transfer(proposer, payout);
        }

        emit Finalized(rootId);
    }

    function _verifierFor(WorldChainProofLib.ProofLane lane) internal view returns (IWorldChainProofVerifier) {
        if (lane == WorldChainProofLib.ProofLane.VALIDITY_PROOF) return validityProofVerifier;
        if (lane == WorldChainProofLib.ProofLane.TEE_ATTESTATION) return teeVerifier;
        return securityCouncil;
    }

    function _transfer(address payable recipient, uint256 amount) internal {
        if (amount == 0) return;
        (bool ok,) = recipient.call{value: amount}("");
        if (!ok) revert TransferFailed(recipient, amount);
    }
}
