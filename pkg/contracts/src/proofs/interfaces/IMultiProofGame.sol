// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ProofLib} from "../lib/ProofLib.sol";
import {IWorldChainProofVerifier} from "./IWorldChainProofVerifier.sol";
import {IWorldChainStakingRegistry} from "./IWorldChainStakingRegistry.sol";

import {BondDistributionMode} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";
import {IDisputeGameFactory} from "@optimism-bedrock/interfaces/dispute/IDisputeGameFactory.sol";
import {IAnchorStateRegistry} from "@optimism-bedrock/interfaces/dispute/IAnchorStateRegistry.sol";
import {IDelayedWETH} from "@optimism-bedrock/interfaces/dispute/IDelayedWETH.sol";

/// @notice The WIP-1006 proof-lane extensions layered on top of the stock `IDisputeGame`
///         surface. Only the members `IDisputeGame` does not already declare live here;
///         proof-lane verifiers and offchain services need them to bind a proof to a game.
interface IMultiProofGame is IDisputeGame {
    ////////////////////////////////////////////////////////////////
    //                         Structs                            //
    ////////////////////////////////////////////////////////////////

    /// @notice Per-deployment configuration, fixed as immutables on the implementation.
    /// @dev The implementation is registered with empty DGF implementation args, so none of
    ///      this configuration rides in the CWIA payload.
    struct GameConfig {
        ProofLib.Domain domain;
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
    error InvalidProof(ProofLib.ProofLane lane, bytes32 rootId);
    error InvalidDomainHash(bytes32 expected, bytes32 actual);
    error InvalidL1Head(bytes32 l1OriginHash, uint256 l1OriginNumber);
    error InvalidBlockNumber(uint256 blockNumber);
    error InvalidRetryReference(uint256 attempt, address retryOf);
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

    /// @notice Emitted when a staked challenger disputes the proposal.
    event Challenged(address indexed challenger, uint64 proofDeadline);

    /// @notice Emitted when a proof lane is accepted for the first time.
    event ProofLaneSupported(ProofLib.ProofLane indexed lane, bytes32 indexed rootId, uint8 proofBitmap);

    /// @notice Emitted once, on the transition to settlement-ready.
    event ProofThresholdReached(bytes32 indexed rootId, uint8 proofBitmap);

    /// @notice Emitted when a lane that already counts toward the threshold is resubmitted.
    event DuplicateProofLane(ProofLib.ProofLane indexed lane, bytes32 indexed rootId, uint8 proofBitmap);

    /// @notice Emitted when the bond distribution mode is locked in.
    event GameClosed(BondDistributionMode bondDistributionMode);

    ////////////////////////////////////////////////////////////////
    //                   Deployment parameters                    //
    ////////////////////////////////////////////////////////////////

    /// @notice Number of distinct proof lanes required to finalize a challenged root.
    function PROOF_THRESHOLD() external view returns (uint8);

    /// @notice Total number of proof lanes defined by the protocol.
    function PROOF_LANE_COUNT() external view returns (uint8);

    /// @notice Domain parameters this deployment proves against.
    function domain() external view returns (ProofLib.Domain memory);

    /// @notice Hash of the deployment's domain parameters.
    function domainHash() external view returns (bytes32);

    /// @notice Seconds a proposal may be challenged after creation.
    function challengePeriod() external view returns (uint64);

    /// @notice Seconds a challenged proposal has to reach the proof threshold.
    function proofPeriod() external view returns (uint64);

    /// @notice Bond required to create a proposal.
    function proposerBond() external view returns (uint256);

    /// @notice Bond required to challenge a proposal.
    function challengerBond() external view returns (uint256);

    /// @notice Verifier backing the validity-proof lane.
    function validityProofVerifier() external view returns (IWorldChainProofVerifier);

    /// @notice Verifier backing the TEE-attestation lane.
    function teeVerifier() external view returns (IWorldChainProofVerifier);

    /// @notice Verifier backing the security-council lane.
    function securityCouncil() external view returns (IWorldChainProofVerifier);

    /// @notice Registry that gates who may challenge.
    function stakingRegistry() external view returns (IWorldChainStakingRegistry);

    /// @notice Factory that created this clone and the only permitted initializer.
    function disputeGameFactory() external view returns (IDisputeGameFactory);

    /// @notice Registry providing the anchor root, blacklist, and finality airgap.
    function anchorStateRegistry() external view returns (IAnchorStateRegistry);

    /// @notice Bond custody contract.
    function weth() external view returns (IDelayedWETH);

    ////////////////////////////////////////////////////////////////
    //                      Proposal context                      //
    ////////////////////////////////////////////////////////////////

    /// @notice The proposal transition identifier bound by every proof lane.
    function rootId() external view returns (bytes32);

    /// @notice Domain hash carried in the CWIA payload, checked against `domainHash`.
    function proposalDomainHash() external view returns (bytes32);

    /// @notice Retry nonce for this transition. Attempt N requires attempt N-1 to have timed
    ///         out on proofs or to have been created before this game type became respected.
    function attempt() external view returns (uint256);

    /// @notice Previous attempt for this transition, or the zero address for attempt zero.
    function retryOf() external view returns (address);

    /// @notice Proof verified when the proposal was created.
    function creationProof() external view returns (bytes memory);

    /// @notice Lane through which `creationProof` was verified.
    function creationProofLane() external view returns (ProofLib.ProofLane);

    /// @notice Parent game, or the anchor registry when the proposal starts from its current root.
    function parentRef() external view returns (address);

    /// @notice Output root this proposal starts from.
    function startingRootClaim() external view returns (bytes32);

    /// @notice L2 block number this proposal starts from.
    function startingL2BlockNumber() external view returns (uint256);

    /// @notice Alias of `l2SequenceNumber` retained for proof-lane and offchain consumers.
    function l2BlockNumber() external view returns (uint256);

    /// @notice Alias of `l1Head` retained for proof-lane and offchain consumers.
    function l1OriginHash() external view returns (bytes32);

    /// @notice L1 block number of `l1Head`.
    function l1OriginNumber() external view returns (uint256);

    ////////////////////////////////////////////////////////////////
    //                       Game progress                        //
    ////////////////////////////////////////////////////////////////

    /// @notice Derived legacy state machine view.
    function state() external view returns (ProofLib.RootState);

    /// @notice Why the game was invalidated, if it was.
    function invalidationReason() external view returns (ProofLib.InvalidationReason);

    /// @notice Bitmap of the proof lanes that count toward the threshold.
    function proofBitmap() external view returns (uint8);

    /// @notice Number of set lanes in `proofBitmap`.
    function proofCount() external view returns (uint8);

    /// @notice Challenger that disputed this proposal, or the zero address.
    function challenger() external view returns (address payable);

    /// @notice Timestamp after which the proposal can no longer be challenged.
    function challengeDeadline() external view returns (uint64);

    /// @notice Timestamp after which a challenged proposal can no longer gain proof lanes.
    function proofDeadline() external view returns (uint64);

    /// @notice Timestamp of the challenge, or zero.
    function challengedAt() external view returns (uint64);

    /// @notice Resolution timestamp when the defender won, otherwise zero.
    function finalizedAt() external view returns (uint64);

    /// @notice Resolution timestamp when the challenger won, otherwise zero.
    function invalidatedAt() external view returns (uint64);

    /// @notice Returns whether this game can resolve now and the resulting legacy outcome.
    function resolutionStatus()
        external
        view
        returns (bool resolvable, ProofLib.RootState outcome, ProofLib.InvalidationReason reason);

    ////////////////////////////////////////////////////////////////
    //                    Challenge and proofs                    //
    ////////////////////////////////////////////////////////////////

    /// @notice Disputes the proposal. Requires a staked caller, an open challenge window, and
    ///         exactly `challengerBond`.
    function challenge() external payable;

    /// @notice Submits `proof` for `laneId`. No-ops when the lane already counts.
    function submitProofLane(uint8 laneId, bytes calldata proof) external;

    ////////////////////////////////////////////////////////////////
    //                      Bond settlement                       //
    ////////////////////////////////////////////////////////////////

    /// @notice Finalizes bond distribution after the registry's finality airgap and attempts
    ///         to advance the anchor to this game.
    function closeGame() external;

    /// @notice Distribution mode locked in by `closeGame`.
    function bondDistributionMode() external view returns (BondDistributionMode);

    /// @notice Total bonds custodied by this game.
    function totalBonds() external view returns (uint256);

    /// @notice Credit owed to `recipient` when the game closes in `NORMAL` mode.
    function normalModeCredit(address recipient) external view returns (uint256 amount);

    /// @notice Credit owed to `recipient` when the game closes in `REFUND` mode.
    function refundModeCredit(address recipient) external view returns (uint256 amount);

    /// @notice Returns the credit `recipient` can claim from this game.
    function credit(address recipient) external view returns (uint256);

    /// @notice Permissionlessly claims `recipient`'s credit via the two-phase DelayedWETH flow;
    ///         the caller cannot redirect funds away from `recipient`.
    function claimCredit(address recipient) external;
}
