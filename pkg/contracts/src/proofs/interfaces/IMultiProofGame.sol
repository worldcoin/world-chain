// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ProofLib} from "../lib/ProofLib.sol";
import {IWorldChainProofVerifier} from "./IWorldChainProofVerifier.sol";
import {IWorldChainStakingRegistry} from "./IWorldChainStakingRegistry.sol";

import {BondDistributionMode, Duration, Hash, Timestamp} from "@optimism-bedrock/src/dispute/lib/Types.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";
import {IDisputeGameFactory} from "@optimism-bedrock/interfaces/dispute/IDisputeGameFactory.sol";
import {IAnchorStateRegistry} from "@optimism-bedrock/interfaces/dispute/IAnchorStateRegistry.sol";
import {IDelayedWETH} from "@optimism-bedrock/interfaces/dispute/IDelayedWETH.sol";

/// @title IMultiProofGame
/// @author World Contributors
/// @custom:security-contact security@toolsforhumanity.com
interface IMultiProofGame is IDisputeGame {
    ////////////////////////////////////////////////////////////////
    //                         Enums                              //
    ////////////////////////////////////////////////////////////////

    /// @notice The lifecycle of the proposer's claim.
    enum ProposalStatus {
        // The initial state of a new proposal.
        Unchallenged,
        // A proposal that has been challenged but not yet proven.
        Challenged,
        // An unchallenged proposal supported by at least one accepted proof lane.
        UnchallengedAndValidProofProvided,
        // A challenged proposal supported by `PROOF_THRESHOLD` distinct proof lanes.
        ChallengedAndValidProofProvided,
        // The final state after resolution, either GameStatus.CHALLENGER_WINS or GameStatus.DEFENDER_WINS.
        Resolved
    }

    ////////////////////////////////////////////////////////////////
    //                         Structs                            //
    ////////////////////////////////////////////////////////////////

    /// @notice The `ClaimData` struct represents the data associated with the root claim.
    struct ClaimData {
        ProposalStatus status; // 1 byte   \
        address challenger; // 20 bytes  |
        Timestamp deadline; // 8 bytes   |-- one slot (31 bytes)
        uint8 proofBitmap; // 1 byte    |
        ProofLib.InvalidationReason invalidationReason; // 1 byte    /
    }

    /// @notice Per-deployment configuration, fixed as immutables on the implementation.
    /// @dev The implementation is registered with empty DGF implementation args, so none of
    ///      this configuration rides in the CWIA payload.
    struct GameConfig {
        uint256 proofSystemVersion;
        bytes32 rollupConfigHash;
        uint256 blockInterval;
        uint64 challengePeriod;
        uint64 proofPeriod;
        uint256 proposerBond;
        uint256 challengerBond;
        address protocolFeeRecipient;
        uint8 proofThreshold;
        IWorldChainProofVerifier validityProofVerifier;
        IWorldChainProofVerifier teeVerifier;
        IWorldChainProofVerifier securityCouncil;
        IWorldChainStakingRegistry stakingRegistry;
        IAnchorStateRegistry anchorStateRegistry;
        IDelayedWETH weth;
    }

    ////////////////////////////////////////////////////////////////
    //                         Errors                             //
    ////////////////////////////////////////////////////////////////

    error InvalidActivationParameters();
    error NotDisputeGameFactory(address caller);
    error InvalidL2BlockNumber(uint256 expectedL2BlockNumber, uint256 actualL2BlockNumber);
    error GameNotRetryable(bytes32 uuidPreimageHash);
    error UnstakedChallenger(address challenger);
    error InvalidLane(uint8 lane);
    error InvalidProof(ProofLib.ProofLane lane, bytes32 rootId);
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

    /// @notice Commitment binding this deployment to its chain, proof-system version, rollup
    ///         configuration, and proposal cadence.
    function domainHash() external view returns (bytes32);

    /// @notice Hash of the rollup configuration the proof lanes verify against.
    function rollupConfigHash() external view returns (bytes32);

    /// @notice Number of L2 blocks each proposal must extend its parent by.
    function blockInterval() external view returns (uint256);

    /// @notice Seconds a proposal may be challenged after creation.
    function challengePeriod() external view returns (Duration);

    /// @notice Seconds after creation a challenged proposal has to reach the proof threshold.
    function proofPeriod() external view returns (Duration);

    /// @notice Bond required to create a proposal.
    function proposerBond() external view returns (uint256);

    /// @notice Bond required to challenge a proposal.
    function challengerBond() external view returns (uint256);

    /// @notice Recipient of proposer bonds forfeited by proofless unchallenged timeouts.
    function protocolFeeRecipient() external view returns (address);

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

    /// @notice Parent game, or the anchor registry when no compatible anchor game exists.
    function parentRef() external view returns (address);

    /// @notice The output root and L2 block number this proposal starts from.
    function startingProposal() external view returns (Hash root, uint256 l2SequenceNumber);

    /// @notice Only the starting block number of the game.
    function startingBlockNumber() external view returns (uint256);

    /// @notice Starting output root of the game.
    function startingRootHash() external view returns (Hash);

    /// @notice L1 block number of `l1Head`.
    function l1OriginNumber() external view returns (uint256);

    ////////////////////////////////////////////////////////////////
    //                       Game progress                        //
    ////////////////////////////////////////////////////////////////

    /// @notice The claim state, following the `ProposalStatus` state machine.
    function claimData()
        external
        view
        returns (
            ProposalStatus status,
            address challenger,
            Timestamp deadline,
            uint8 proofBitmap,
            ProofLib.InvalidationReason invalidationReason
        );

    /// @notice Derived legacy state machine view.
    function state() external view returns (ProofLib.RootState);

    /// @notice Why the game was invalidated, if it was.
    function invalidationReason() external view returns (ProofLib.InvalidationReason);

    /// @notice Bitmap of the proof lanes that count toward the threshold.
    function proofBitmap() external view returns (uint8);

    /// @notice Challenger that disputed this proposal, or the zero address.
    function challenger() external view returns (address);

    /// @notice Timestamp after which the proposal can no longer be challenged.
    function challengeDeadline() external view returns (Timestamp);

    /// @notice Timestamp after which a challenged proposal can no longer gain proof lanes.
    function proofDeadline() external view returns (Timestamp);

    /// @notice True once the game can no longer change outcome: the active deadline has
    ///         passed or the proof threshold has been reached.
    function gameOver() external view returns (bool);

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
    function challenge() external payable returns (ProposalStatus);

    /// @notice Submits `proof` for `laneId`. Before a challenge, any accepted lane satisfies the
    ///         initial proof requirement; after a challenge, distinct lanes count toward the
    ///         configured threshold. No-ops when the lane already counts.
    function submitProofLane(uint8 laneId, bytes calldata proof) external returns (ProposalStatus);

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
