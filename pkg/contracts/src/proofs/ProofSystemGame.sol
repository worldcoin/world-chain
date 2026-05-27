// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {IProofLaneVerifier} from "./interfaces/IProofLaneVerifier.sol";
import {IStakingRegistry} from "./interfaces/IStakingRegistry.sol";
import {ProofRootIdLib} from "./libraries/ProofRootIdLib.sol";
import {DomainConfig, ProofLane, RootCommitment, RootState} from "./ProofSystemTypes.sol";

contract ProofSystemGame {
    uint256 public constant PROOF_THRESHOLD = 2;
    uint256 public constant PROOF_LANE_COUNT = 4;

    DomainConfig public domainConfig;
    RootCommitment public rootCommitment;

    IProofLaneVerifier[4] public laneVerifiers;
    IStakingRegistry public immutable stakingRegistry;

    bytes32 public immutable domainHash;
    bytes32 public immutable rootId;
    uint256 public immutable challengePeriod;
    uint256 public immutable bondAmount;

    uint64 public immutable createdAt;
    uint64 public immutable challengeDeadline;

    RootState public state;
    uint8 public proofBitmap;
    uint8 public proofCount;

    address[] public challengers;

    error ChallengeDeadlineElapsed();
    error ChallengePeriodActive();
    error ChallengerNotStaked(address challenger);
    error DuplicateProofLane(ProofLane lane);
    error InvalidChallengePeriod();
    error InvalidLane(uint8 lane);
    error InvalidProof(ProofLane lane, bytes32 rootId);
    error InvalidState(RootState current);
    error MissingLaneVerifier(uint256 lane);
    error MissingStakingRegistry();

    event RootChallenged(bytes32 indexed rootId, address indexed challenger, uint256 bondAmount);
    event RootFinalized(bytes32 indexed rootId, bool challenged);
    event RootInvalidated(bytes32 indexed rootId, ProofLane indexed lane);
    event RootProofAccepted(bytes32 indexed rootId, ProofLane indexed lane, uint8 proofBitmap);

    constructor(
        DomainConfig memory domainConfig_,
        RootCommitment memory rootCommitment_,
        IProofLaneVerifier[4] memory laneVerifiers_,
        IStakingRegistry stakingRegistry_,
        uint256 challengePeriod_,
        uint256 bondAmount_
    ) {
        if (challengePeriod_ == 0) revert InvalidChallengePeriod();
        if (address(stakingRegistry_) == address(0)) revert MissingStakingRegistry();

        for (uint256 i = 0; i < PROOF_LANE_COUNT; ++i) {
            if (address(laneVerifiers_[i]) == address(0)) revert MissingLaneVerifier(i);
            laneVerifiers[i] = laneVerifiers_[i];
        }

        domainConfig = domainConfig_;
        rootCommitment = rootCommitment_;
        stakingRegistry = stakingRegistry_;
        challengePeriod = challengePeriod_;
        bondAmount = bondAmount_;

        bytes32 computedDomainHash = ProofRootIdLib.hashDomain(domainConfig_);
        domainHash = computedDomainHash;
        rootId = ProofRootIdLib.hashRootId(computedDomainHash, rootCommitment_);

        createdAt = uint64(block.timestamp);
        challengeDeadline = uint64(block.timestamp + challengePeriod_);
        state = RootState.PROPOSED;
    }

    function challenge() external {
        if (state != RootState.PROPOSED && state != RootState.CHALLENGED) revert InvalidState(state);
        if (block.timestamp >= challengeDeadline) revert ChallengeDeadlineElapsed();
        if (!stakingRegistry.isStaked(msg.sender)) revert ChallengerNotStaked(msg.sender);

        stakingRegistry.lockChallengeBond(msg.sender, rootId, bondAmount);
        challengers.push(msg.sender);

        if (state == RootState.PROPOSED) {
            state = RootState.CHALLENGED;
        }

        emit RootChallenged(rootId, msg.sender, bondAmount);
    }

    function finalizeUnchallenged() external {
        if (state != RootState.PROPOSED) revert InvalidState(state);
        if (block.timestamp < challengeDeadline) revert ChallengePeriodActive();

        _finalize(false);
    }

    function submitProof(ProofLane lane, bytes calldata proof) external {
        if (state != RootState.CHALLENGED) revert InvalidState(state);

        uint8 mask = _laneMask(lane);
        if (proofBitmap & mask != 0) revert DuplicateProofLane(lane);

        IProofLaneVerifier verifier = laneVerifiers[uint8(lane)];
        if (!verifier.verify(rootId, proof)) revert InvalidProof(lane, rootId);

        proofBitmap |= mask;
        proofCount += 1;

        emit RootProofAccepted(rootId, lane, proofBitmap);

        if (proofCount >= PROOF_THRESHOLD) {
            _finalize(true);
        }
    }

    function submitInvalidity(ProofLane lane, bytes calldata proof) external {
        if (state != RootState.PROPOSED && state != RootState.CHALLENGED) revert InvalidState(state);

        IProofLaneVerifier verifier = laneVerifiers[uint8(lane)];
        if (!verifier.verifyInvalidity(rootId, proof)) revert InvalidProof(lane, rootId);

        state = RootState.INVALIDATED;
        for (uint256 i = 0; i < challengers.length; ++i) {
            stakingRegistry.refundChallengeBond(challengers[i], rootId);
        }

        emit RootInvalidated(rootId, lane);
    }

    function challengerCount() external view returns (uint256) {
        return challengers.length;
    }

    function _finalize(bool challenged) internal {
        state = RootState.FINALIZED;

        if (challenged) {
            for (uint256 i = 0; i < challengers.length; ++i) {
                stakingRegistry.forfeitChallengeBond(challengers[i], rootId);
            }
        }

        emit RootFinalized(rootId, challenged);
    }

    function _laneMask(ProofLane lane) internal pure returns (uint8) {
        uint8 laneIndex = uint8(lane);
        if (laneIndex >= PROOF_LANE_COUNT) revert InvalidLane(laneIndex);
        return uint8(1 << laneIndex);
    }
}
