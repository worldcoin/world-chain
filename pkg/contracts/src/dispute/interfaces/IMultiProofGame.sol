// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {LibProof, Bitmap} from "../lib/LibProof.sol";
import {IWorldChainProofVerifier} from "./IWorldChainProofVerifier.sol";

import {BondDistributionMode, Duration, GameStatus, Hash, Timestamp} from "@optimism-bedrock/src/dispute/lib/Types.sol";
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
        ProposalStatus status; // 1 byte                            |
        address challenger; // 20 bytes                             |
        Timestamp deadline; // 8 bytes                              |-- one slot (31 bytes)
        Bitmap proofBitmap; // 1 byte                      |
        LibProof.InvalidationReason invalidationReason; // 1 byte   |
    }

    /// @notice Per-deployment configuration, fixed as immutables on the implementation.
    /// @dev The implementation is registered with empty DGF implementation args, so none of
    ///      this configuration rides in the CWIA payload.
    struct GameConfig {
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
    error InvalidLane(uint8 lane);
    error InvalidProof(LibProof.ProofLane lane, bytes32 rootId);
    error InvalidDomainHash(bytes32 expected, bytes32 actual);
    error InconsistentSystemConfiguration();

    /// @notice Thrown when a lane that already counts toward the threshold is resubmitted.
    error DuplicateProofLane(LibProof.ProofLane lane, bytes32 rootId, Bitmap proofBitmap);

    ////////////////////////////////////////////////////////////////
    //                         Events                             //
    ////////////////////////////////////////////////////////////////

    /// @notice Emitted when a staked challenger disputes the proposal.
    event Challenged(address indexed challenger, uint64 proofDeadline);

    /// @notice Emitted when a proof lane is accepted; `proofBitmap` carries the updated lane
    ///         set, from which indexers can derive threshold status.
    event Proved(LibProof.ProofLane indexed lane, bytes32 indexed rootId, address recipient, Bitmap proofBitmap);

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

    /// @notice Number of L2 blocks each proposal must advance from its parent.
    function blockInterval() external view returns (uint256);

    /// @notice Seconds after creation during which a proposal may be challenged.
    function challengePeriod() external view returns (Duration);

    /// @notice Seconds after creation a challenged proposal has to reach the proof threshold.
    function proofPeriod() external view returns (Duration);

    /// @notice Bond required to create a proposal.
    function proposerBond() external view returns (uint256);

    /// @notice Bond required to challenge a proposal.
    function challengerBond() external view returns (uint256);

    /// @notice Share of a forfeited proposer bond paid to a winning challenger, in basis points.
    function CHALLENGER_REWARD_BPS() external view returns (uint256);

    /// @notice Recipient of the share of forfeited proposer bonds not paid to a challenger.
    function protocolFeeRecipient() external view returns (address);

    /// @notice Verifier backing the validity-proof lane.
    function validityProofVerifier() external view returns (IWorldChainProofVerifier);

    /// @notice Verifier backing the TEE-attestation lane.
    function teeVerifier() external view returns (IWorldChainProofVerifier);

    /// @notice Verifier backing the security-council lane.
    function securityCouncil() external view returns (IWorldChainProofVerifier);

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
            Bitmap proofBitmap,
            LibProof.InvalidationReason invalidationReason
        );

    /// @notice Why the game was invalidated, if it was.
    function invalidationReason() external view returns (LibProof.InvalidationReason);

    /// @notice Bitmap of the proof lanes that count toward the threshold.
    function proofBitmap() external view returns (Bitmap);

    /// @notice Reward recipient recorded for `laneId`, or the zero address if unproven.
    function laneRecipient(uint8 laneId) external view returns (address);

    /// @notice Challenger that disputed this proposal, or the zero address.
    function challenger() external view returns (address);

    /// @notice Timestamp after which the proposal can no longer be challenged.
    function challengeDeadline() external view returns (Timestamp);

    /// @notice Timestamp after which a challenged proposal can no longer gain proof lanes.
    function proofDeadline() external view returns (Timestamp);

    /// @notice True once the game can no longer change outcome: the active deadline has
    ///         passed or the proof threshold has been reached.
    function gameOver() external view returns (bool);

    /// @notice Returns whether this game can resolve now and the outcome a resolve call would
    ///         produce; `outcome` is `IN_PROGRESS` while the game cannot resolve.
    function resolutionStatus()
        external
        view
        returns (bool resolvable, GameStatus outcome, LibProof.InvalidationReason reason);

    ////////////////////////////////////////////////////////////////
    //                    Challenge and proofs                    //
    ////////////////////////////////////////////////////////////////

    /// @notice Disputes a proven proposal during the open challenge window for exactly `challengerBond`.
    function challenge() external payable returns (ProposalStatus);

    /// @notice Submits a compact proof payload. Before a challenge, any accepted lane satisfies
    ///         the initial proof requirement; after a challenge, distinct lanes count toward the
    ///         configured threshold. Reverts when the lane already counts.
    function submitProofLane(bytes calldata proof) external returns (ProposalStatus);

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
