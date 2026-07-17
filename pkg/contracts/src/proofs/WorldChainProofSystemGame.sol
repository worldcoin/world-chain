// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {WorldChainProofLib} from "./WorldChainProofLib.sol";
import {IWorldChainProofVerifier} from "./interfaces/IWorldChainProofVerifier.sol";
import {IWorldChainStakingRegistry} from "./interfaces/IWorldChainStakingRegistry.sol";

contract WorldChainProofSystemGame {
    using WorldChainProofLib for uint8;

    struct ProposalInit {
        address proposer;
        address parentRef;
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

    error InvalidBond(uint256 expected, uint256 actual);
    error InvalidState(WorldChainProofLib.RootState expected, WorldChainProofLib.RootState actual);
    error ChallengePeriodElapsed(uint256 timestamp, uint256 challengeDeadline);
    error ProofPeriodElapsed(uint256 timestamp, uint256 proofDeadline);
    error ProofPeriodOpen(uint256 timestamp, uint256 proofDeadline);
    error ChallengePeriodOpen(uint256 timestamp, uint256 challengeDeadline);
    error UnstakedChallenger(address challenger);
    error DuplicateChallenge(address challenger);
    error InvalidLane(uint8 lane);
    error InvalidProof(WorldChainProofLib.ProofLane lane, bytes32 rootId);
    error TransferFailed(address recipient, uint256 amount);

    event Challenged(address indexed challenger, uint64 proofDeadline);
    event ProofLaneSupported(WorldChainProofLib.ProofLane indexed lane, bytes32 indexed rootId, uint8 proofBitmap);
    event DuplicateProofLane(WorldChainProofLib.ProofLane indexed lane, bytes32 indexed rootId, uint8 proofBitmap);
    event Finalized(bytes32 indexed rootId);
    event Invalidated(bytes32 indexed rootId);

    /// Number of distinct proof lanes required to finalize a challenged root.
    /// Set per-deployment by the factory (defaults to `WorldChainProofLib.PROOF_THRESHOLD`).
    uint8 public immutable PROOF_THRESHOLD;
    uint8 public constant PROOF_LANE_COUNT = WorldChainProofLib.PROOF_LANE_COUNT;

    address payable public immutable proposer;
    address public immutable parentRef;
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

    address[] public challengers;
    mapping(address challenger => uint256 amount) public challengerBonds;

    constructor(ProposalInit memory proposal, ActivationConfig memory config) payable {
        if (msg.value != config.proposerBond) revert InvalidBond(config.proposerBond, msg.value);

        proposer = payable(proposal.proposer);
        parentRef = proposal.parentRef;
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

        proofBitmap |= mask;
        emit ProofLaneSupported(lane, rootId, proofBitmap);

        if (WorldChainProofLib.hasThreshold(proofBitmap, PROOF_THRESHOLD)) {
            _finalize();
        }
    }

    function finalize() external {
        if (state == WorldChainProofLib.RootState.PROPOSED) {
            if (block.timestamp < challengeDeadline) {
                revert ChallengePeriodOpen(block.timestamp, challengeDeadline);
            }
            _finalize();
            return;
        }

        if (state == WorldChainProofLib.RootState.CHALLENGED) {
            if (!WorldChainProofLib.hasThreshold(proofBitmap, PROOF_THRESHOLD)) {
                revert InvalidState(WorldChainProofLib.RootState.FINALIZED, state);
            }
            _finalize();
            return;
        }

        revert InvalidState(WorldChainProofLib.RootState.PROPOSED, state);
    }

    function invalidate() external {
        if (state != WorldChainProofLib.RootState.CHALLENGED) {
            revert InvalidState(WorldChainProofLib.RootState.CHALLENGED, state);
        }
        if (block.timestamp < proofDeadline) {
            revert ProofPeriodOpen(block.timestamp, proofDeadline);
        }
        if (WorldChainProofLib.hasThreshold(proofBitmap, PROOF_THRESHOLD)) {
            revert InvalidState(WorldChainProofLib.RootState.INVALIDATED, state);
        }

        state = WorldChainProofLib.RootState.INVALIDATED;
        invalidatedAt = uint64(block.timestamp);

        for (uint256 i = 0; i < challengers.length; i++) {
            address challenger = challengers[i];
            uint256 amount = challengerBonds[challenger];
            challengerBonds[challenger] = 0;
            _transfer(payable(challenger), amount);
        }

        uint256 forfeited = address(this).balance;
        if (forfeited != 0 && challengers.length != 0) {
            _transfer(payable(challengers[0]), forfeited);
        } else if (forfeited != 0) {
            _transfer(proposer, forfeited);
        }

        emit Invalidated(rootId);
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
