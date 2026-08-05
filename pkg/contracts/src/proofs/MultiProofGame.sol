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
    Duration,
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
    GameOver,
    GamePaused,
    IncorrectBondAmount,
    InvalidBondDistributionMode,
    InvalidParentGame,
    InvalidProposalStatus,
    NoCreditToClaim,
    ParentGameNotResolved,
    UnexpectedRootClaim
} from "@optimism-bedrock/src/dispute/lib/Errors.sol";
import {IDisputeGame} from "@optimism-bedrock/interfaces/dispute/IDisputeGame.sol";
import {IDisputeGameFactory} from "@optimism-bedrock/interfaces/dispute/IDisputeGameFactory.sol";
import {IAnchorStateRegistry} from "@optimism-bedrock/interfaces/dispute/IAnchorStateRegistry.sol";
import {IDelayedWETH} from "@optimism-bedrock/interfaces/dispute/IDelayedWETH.sol";
import {ISemver} from "@optimism-bedrock/interfaces/universal/ISemver.sol";

/// @title MultiProofGame
/// @author World Contributors
/// @notice A Multi Proof `IDisputeGame` supporing 3 different proof 'lanes'.
///     1.) ZK Validity Proof via Succinct Prover
///     2.) AWS Nitro Enclave Attestation
///     3.) Security Council Attestations
///
/// @dev Additional Proof Lanes may be added in the future for this game type.
/// @custom:security-contact security@toolsforhumanity.com
contract MultiProofGame is Clone, ISemver, IMultiProofGame {
    ////////////////////////////////////////////////////////////////
    //                         State Vars                         //
    ////////////////////////////////////////////////////////////////

    /// @notice Semantic version.
    /// @custom:semver 0.1.0
    string public constant version = "0.1.0";

    /// @notice Number of distinct proof lanes required to finalize a challenged root.
    uint8 public immutable PROOF_THRESHOLD;

    /// @notice Total number of proof lanes defined by the protocol.
    uint8 public constant PROOF_LANE_COUNT = ProofLib.PROOF_LANE_COUNT;

    /// @notice Domain parameters this deployment proves against, exposed through `domain()`.
    uint256 internal immutable DOMAIN_CHAIN_ID;
    uint256 internal immutable DOMAIN_PROOF_SYSTEM_VERSION;
    bytes32 internal immutable DOMAIN_ROLLUP_CONFIG_HASH;
    uint256 internal immutable DOMAIN_BLOCK_INTERVAL;

    /// @notice Hash of the deployment's domain parameters.
    bytes32 public immutable domainHash;

    /// @notice Seconds a proposal may be challenged after creation.
    Duration public immutable challengePeriod;

    /// @notice Seconds after creation a challenged proposal has to reach the proof threshold.
    Duration public immutable proofPeriod;

    /// @notice Bond required to create a proposal.
    uint256 public immutable proposerBond;

    /// @notice Bond required to challenge a proposal.
    uint256 public immutable challengerBond;

    /// @notice Protocol-controlled recipient of proof-timeout forfeitures and challenge fees.
    address public immutable protocolFeeRecipient;

    /// @notice Non-refundable portion of the challenger bond charged whenever a game is challenged.
    uint256 public immutable challengeFee;

    /// @notice Verifiers backing the proof lanes, indexed by `ProofLib.ProofLane`.
    IWorldChainProofVerifier public immutable validityProofVerifier;
    IWorldChainProofVerifier public immutable teeVerifier;
    IWorldChainProofVerifier public immutable securityCouncil;

    /// @notice Registry that gates who may challenge.
    IWorldChainStakingRegistry public immutable stakingRegistry;

    /// @notice Factory that created this clone and the only permitted initializer.
    IDisputeGameFactory public immutable disputeGameFactory;

    /// @notice Registry providing the anchor root, blacklist, and finality airgap.
    IAnchorStateRegistry public immutable anchorStateRegistry;

    /// @notice The DelayedWETH contract used for bond custody.
    IDelayedWETH public immutable weth;

    /// @notice The starting timestamp of the game.
    Timestamp public createdAt;

    /// @notice The timestamp of the game's global resolution.
    Timestamp public resolvedAt;

    /// @notice The current status of the game.
    GameStatus public status;

    /// @notice Flag for the `initialize` function to prevent re-initialization.
    bool internal initialized;

    /// @notice L1 block number of `l1Head`, captured at creation for the `rootId` preimage.
    uint64 internal _l1OriginNumber;

    /// @notice The claim made by the proposer.
    ClaimData public claimData;

    /// @notice Credited balances for winning participants.
    mapping(address recipient => uint256 amount) public normalModeCredit;

    /// @notice A mapping of each claimant's refund mode credit.
    mapping(address recipient => uint256 amount) public refundModeCredit;

    /// @notice The starting output root of the game that is proven from in case of a challenge.
    Proposal public startingProposal;

    /// @notice The bond distribution mode of the game.
    BondDistributionMode public bondDistributionMode;

    /// @notice A boolean for whether or not the game type was respected when the game was created.
    bool public wasRespectedGameTypeWhenCreated;

    /// @notice The total bonds deposited into the game.
    uint256 public totalBonds;

    /// @notice The proposal transition identifier bound by every proof lane.
    bytes32 public rootId;

    ////////////////////////////////////////////////////////////////
    //                        Constructor                         //
    ////////////////////////////////////////////////////////////////

    constructor(GameConfig memory config) {
        if (
            config.challengePeriod == 0 || config.proofPeriod <= config.challengePeriod || config.domain.chainId == 0
                || config.domain.proofSystemVersion == 0 || config.domain.blockInterval == 0
                || config.proofThreshold == 0 || config.proofThreshold > ProofLib.PROOF_LANE_COUNT
                || config.protocolFeeRecipient == address(0) || address(config.disputeGameFactory) == address(0)
                || config.challengeFee == 0 || config.challengeFee > config.challengerBond
                || config.challengeFee >= config.proposerBond || address(config.anchorStateRegistry) == address(0)
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
        challengePeriod = Duration.wrap(config.challengePeriod);
        proofPeriod = Duration.wrap(config.proofPeriod);
        proposerBond = config.proposerBond;
        challengerBond = config.challengerBond;
        protocolFeeRecipient = config.protocolFeeRecipient;
        challengeFee = config.challengeFee;
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
    //                       CWIA GETTERS                         //
    ////////////////////////////////////////////////////////////////

    // CWIA layout appended by `DisputeGameFactory.create` (no implementation args):
    //   [0x00, 0x14) creator address
    //   [0x14, 0x34) root claim
    //   [0x34, 0x54) l1 head (parent block hash at creation)
    //   [0x54, 0xD4) extraData = abi.encode(domainHash, l2BlockNumber, parentRef, attempt)

    /// @notice Getter for the creator of the dispute game.
    function gameCreator() public pure returns (address creator_) {
        creator_ = _getArgAddress(0x00);
    }

    /// @notice Getter for the root claim.
    function rootClaim() public pure returns (Claim rootClaim_) {
        rootClaim_ = Claim.wrap(_getArgBytes32(0x14));
    }

    /// @notice Getter for the parent hash of the L1 block when the dispute game was created.
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

    /// @notice Getter for the extra data.
    function extraData() public pure returns (bytes memory extraData_) {
        extraData_ = _getArgBytes(0x54, 0x80);
    }

    /// @notice Getter for the game type.
    function gameType() public pure returns (GameType gameType_) {
        gameType_ = GameTypes.MULTI_PROOF_GAME_TYPE;
    }

    /// @dev `IDisputeGame` declares this `pure`, so it cannot gate on the `DOMAIN_CHAIN_ID`
    ///      immutable. This matches `ZKDisputeGame`; only super game types carry per-chain
    ///      root claims, and `OptimismPortal2` reads `rootClaim()` for non-super types.
    function rootClaimByChainId(uint256) external pure returns (Claim rootClaim_) {
        rootClaim_ = rootClaim();
    }

    /// @notice Returns the components of the game UUID's preimage provided in the cwia payload.
    function gameData() external pure returns (GameType gameType_, Claim rootClaim_, bytes memory extraData_) {
        gameType_ = gameType();
        rootClaim_ = rootClaim();
        extraData_ = extraData();
    }

    ////////////////////////////////////////////////////////////////
    //                    INITIALIZATION                          //
    ////////////////////////////////////////////////////////////////

    /// @notice Initializes a WIP-1006 clone and validates its domain, parent, interval, retry,
    ///         and bond invariants. Any revert bubbles through `DisputeGameFactory.create`.
    function initialize() external payable {
        // INVARIANT: The game must not have already been initialized.
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

        // INVARIANT: The proposal must target the domain this implementation proves against.
        if (proposalDomainHash() != domainHash) revert InvalidDomainHash(domainHash, proposalDomainHash());

        address parentRef_ = parentRef();

        if (parentRef_ == address(anchorStateRegistry)) {
            // When there is no parent game, the starting output root is the starting anchor.
            startingProposal = anchorStateRegistry.getStartingAnchorRoot();
        } else {
            IDisputeGame parent = IDisputeGame(parentRef_);

            // INVARIANT: The parent must be a registered, respected, live game of this type.
            if (!_isValidGame(parent)) revert InvalidParentGame();

            startingProposal =
                Proposal({root: Hash.wrap(parent.rootClaim().raw()), l2SequenceNumber: parent.l2SequenceNumber()});
        }

        // INVARIANT: Each proposal extends its parent by exactly one block interval.
        uint256 expectedL2BlockNumber = startingProposal.l2SequenceNumber + DOMAIN_BLOCK_INTERVAL;
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

        // Set the root claim.
        claimData = ClaimData({
            status: ProposalStatus.Unchallenged,
            challenger: address(0),
            deadline: Timestamp.wrap(uint64(block.timestamp + challengePeriod.raw())),
            proofBitmap: 0,
            invalidationReason: ProofLib.InvalidationReason.NONE
        });

        // Set the game as initialized.
        initialized = true;

        // Deposit the bond into DelayedWETH and track credits.
        refundModeCredit[gameCreator()] += msg.value;
        totalBonds += msg.value;
        weth.deposit{value: msg.value}();

        // Set the game's starting timestamp.
        createdAt = Timestamp.wrap(uint64(block.timestamp));

        // Set whether the game type was respected when the game was created.
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
    function _isValidGame(IDisputeGame game) internal view returns (bool) {
        return anchorStateRegistry.isGameRegistered(game) && anchorStateRegistry.isGameRespected(game)
            && !anchorStateRegistry.isGameBlacklisted(game) && !anchorStateRegistry.isGameRetired(game)
            && (game.status() != GameStatus.CHALLENGER_WINS);
    }

    ////////////////////////////////////////////////////////////////
    //                    `IDisputeGame` impl                     //
    ////////////////////////////////////////////////////////////////

    /// @notice Challenges the game.
    function challenge() external payable returns (ProposalStatus) {
        // INVARIANT: Cannot challenge if the game is already resolved.
        if (status != GameStatus.IN_PROGRESS) revert ClaimAlreadyResolved();

        // INVARIANT: Cannot challenge if the game is over: the challenge window has closed or
        // the proof threshold already guarantees the defender wins.
        if (gameOver()) revert GameOver();

        // INVARIANT: Can only challenge a game that has not been challenged yet.
        if (claimData.challenger != address(0)) revert ClaimAlreadyChallenged();

        // INVARIANT: Only staked challengers may open a dispute.
        if (!stakingRegistry.isStaked(msg.sender)) revert UnstakedChallenger(msg.sender);

        // If the required bond is not met, revert.
        if (msg.value != challengerBond) revert IncorrectBondAmount();

        // Update the challenger address. Lanes accepted before the challenge keep counting
        // toward the threshold, but the status returns to `Challenged`: the initial proof no
        // longer finalizes on its own.
        claimData.challenger = msg.sender;
        claimData.status = ProposalStatus.Challenged;

        // Move the clock to the proof deadline, which stays anchored at creation rather than
        // at the challenge, so late challenges cannot extend the game.
        claimData.deadline = proofDeadline();

        // The fee is credited in both settlement modes so no later outcome can recycle it.
        normalModeCredit[protocolFeeRecipient] += challengeFee;
        refundModeCredit[protocolFeeRecipient] += challengeFee;
        refundModeCredit[msg.sender] += msg.value - challengeFee;
        totalBonds += msg.value;
        weth.deposit{value: msg.value}();

        emit ChallengeFeeCharged(protocolFeeRecipient, challengeFee);
        emit Challenged(msg.sender, claimData.deadline.raw());

        return claimData.status;
    }

    /// @notice Proves the game through one of the proof lanes.
    /// @param laneId The `ProofLib.ProofLane` being submitted.
    /// @param proof The lane-specific proof bytes binding `rootId`.
    function submitProofLane(uint8 laneId, bytes calldata proof) external returns (ProposalStatus) {
        // INVARIANT: Cannot prove if the game is already resolved.
        if (status != GameStatus.IN_PROGRESS) revert ClaimAlreadyResolved();

        // INVARIANT: Cannot prove if the game is over. Also caps the bitmap at the threshold:
        // once enough lanes have been accepted, further submissions are rejected.
        if (gameOver()) revert GameOver();

        if (laneId >= PROOF_LANE_COUNT) revert InvalidLane(laneId);
        ProofLib.ProofLane lane = ProofLib.ProofLane(laneId);
        uint8 mask = ProofLib.laneMask(lane);

        // No-op on resubmission so racing provers do not revert each other.
        if (claimData.proofBitmap & mask != 0) {
            emit DuplicateProofLane(lane, rootId, claimData.proofBitmap);
            return claimData.status;
        }

        // Verify the proof against the verifier configured for this lane.
        if (!_verifierFor(lane).verify(rootId, proof)) revert InvalidProof(lane, rootId);

        uint8 bitmap = claimData.proofBitmap | mask;
        claimData.proofBitmap = bitmap;
        emit ProofLaneSupported(lane, rootId, bitmap);

        // Update the status of the proposal. Unchallenged, a single lane is a valid proof that
        // finalizes once the challenge window closes; challenged, only the threshold is.
        if (ProofLib.hasThreshold(bitmap, PROOF_THRESHOLD)) {
            // First crossing only: `gameOver()` blocks any submission after the threshold.
            emit ProofThresholdReached(rootId, bitmap);
            claimData.status = claimData.challenger == address(0)
                ? ProposalStatus.UnchallengedAndValidProofProvided
                : ProposalStatus.ChallengedAndValidProofProvided;
        } else if (claimData.challenger == address(0)) {
            claimData.status = ProposalStatus.UnchallengedAndValidProofProvided;
        }

        return claimData.status;
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

    /// @notice Resolves the game after the clock expires or the proof threshold is reached.
    ///         `DEFENDER_WINS` when a proven game expires unchallenged, or when enough proof
    ///         lanes support a challenged claim. `CHALLENGER_WINS` when an applicable proof
    ///         window expires below its requirement, or when the parent game is invalid.
    /// @dev Resolution gates on the parent's *status*, never on its claim validity, so the
    ///      anchor registry's finality airgap does not slow the proposal cadence. Bonds are
    ///      credited here and paid out through `claimCredit` after `closeGame`.
    function resolve() external returns (GameStatus) {
        // INVARIANT: Resolution cannot occur if the game has already been resolved.
        if (status != GameStatus.IN_PROGRESS) revert ClaimAlreadyResolved();

        (GameStatus parentStatus, bool parentBlacklisted) = _parentResolution();

        if (parentBlacklisted || parentStatus == GameStatus.CHALLENGER_WINS) {
            // An invalid parent invalidates this game regardless of its own proof state. Unlike
            // ZKDisputeGame (which awards the challenger), participant funds are refunded
            // because neither party caused the ancestor failure; the protocol fee remains charged.
            status = GameStatus.CHALLENGER_WINS;
            claimData.invalidationReason = ProofLib.InvalidationReason.INVALID_PARENT;
            normalModeCredit[gameCreator()] += proposerBond;
            if (claimData.challenger != address(0)) {
                normalModeCredit[claimData.challenger] += challengerBond - challengeFee;
            }
        } else if (parentStatus == GameStatus.IN_PROGRESS) {
            // INVARIANT: Cannot resolve a game if the parent game has not been resolved.
            revert ParentGameNotResolved();
        } else {
            // INVARIANT: Game must be completed either by clock expiration or the threshold.
            if (!gameOver()) revert GameNotOver();

            // Determine status based on claim status.
            if (claimData.status == ProposalStatus.Unchallenged) {
                // A proofless proposal cannot win merely because nobody challenged it: the
                // proposer forfeits the bond to the protocol.
                status = GameStatus.CHALLENGER_WINS;
                claimData.invalidationReason = ProofLib.InvalidationReason.PROOF_TIMEOUT;
                normalModeCredit[protocolFeeRecipient] += totalBonds;
            } else if (claimData.status == ProposalStatus.Challenged) {
                // A challenged game below threshold times out once its proof window expires.
                // The challenger takes the proposer bond.
                status = GameStatus.CHALLENGER_WINS;
                claimData.invalidationReason = ProofLib.InvalidationReason.PROOF_TIMEOUT;
                normalModeCredit[claimData.challenger] += totalBonds - challengeFee;
            } else if (claimData.status == ProposalStatus.UnchallengedAndValidProofProvided) {
                // Claim is unchallenged and proven: defender wins, game creator gets everything.
                status = GameStatus.DEFENDER_WINS;
                normalModeCredit[gameCreator()] += totalBonds;
            } else if (claimData.status == ProposalStatus.ChallengedAndValidProofProvided) {
                // Claim is challenged but the threshold was reached: defender wins, game
                // creator takes the challenger bond net of the protocol fee.
                status = GameStatus.DEFENDER_WINS;
                normalModeCredit[gameCreator()] += totalBonds - challengeFee;
            } else {
                // This edge case shouldn't be reached, sanity check just in case.
                revert InvalidProposalStatus();
            }
        }

        // Mark the game as resolved.
        claimData.status = ProposalStatus.Resolved;
        resolvedAt = Timestamp.wrap(uint64(block.timestamp));
        emit Resolved(status);

        return status;
    }

    /// @inheritdoc IMultiProofGame
    /// @dev View twin of `resolve()`: reports what a resolve call would do right now.
    function resolutionStatus()
        external
        view
        returns (bool resolvable, ProofLib.RootState outcome, ProofLib.InvalidationReason reason)
    {
        if (status != GameStatus.IN_PROGRESS) {
            return (false, state(), claimData.invalidationReason);
        }

        (GameStatus parentStatus, bool parentBlacklisted) = _parentResolution();
        if (parentBlacklisted || parentStatus == GameStatus.CHALLENGER_WINS) {
            return (true, ProofLib.RootState.INVALIDATED, ProofLib.InvalidationReason.INVALID_PARENT);
        }
        if (parentStatus == GameStatus.IN_PROGRESS || !gameOver()) {
            return (false, state(), ProofLib.InvalidationReason.NONE);
        }

        bool proven = claimData.status == ProposalStatus.UnchallengedAndValidProofProvided
            || claimData.status == ProposalStatus.ChallengedAndValidProofProvided;
        return proven
            ? (true, ProofLib.RootState.FINALIZED, ProofLib.InvalidationReason.NONE)
            : (true, ProofLib.RootState.INVALIDATED, ProofLib.InvalidationReason.PROOF_TIMEOUT);
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

        // Make sure that the game is resolved.
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

    ////////////////////////////////////////////////////////////////
    //                       MISC EXTERNAL                        //
    ////////////////////////////////////////////////////////////////

    /// @notice Determines if the game is finished.
    /// @return gameOver_ True if the active deadline has passed or the threshold is reached.
    function gameOver() public view returns (bool gameOver_) {
        gameOver_ = claimData.deadline.raw() <= uint64(block.timestamp)
            || ProofLib.hasThreshold(claimData.proofBitmap, PROOF_THRESHOLD);
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

    /// @inheritdoc IMultiProofGame
    function domain() external view returns (ProofLib.Domain memory) {
        return ProofLib.Domain({
            chainId: DOMAIN_CHAIN_ID,
            proofSystemVersion: DOMAIN_PROOF_SYSTEM_VERSION,
            rollupConfigHash: DOMAIN_ROLLUP_CONFIG_HASH,
            blockInterval: DOMAIN_BLOCK_INTERVAL
        });
    }

    /// @notice Only the starting block number of the game.
    function startingBlockNumber() external view returns (uint256 startingBlockNumber_) {
        startingBlockNumber_ = startingProposal.l2SequenceNumber;
    }

    /// @notice Starting output root of the game.
    function startingRootHash() external view returns (Hash startingRootHash_) {
        startingRootHash_ = startingProposal.root;
    }

    /// @inheritdoc IMultiProofGame
    function l1OriginNumber() external view returns (uint256) {
        return _l1OriginNumber;
    }

    /// @inheritdoc IMultiProofGame
    function challenger() external view returns (address) {
        return claimData.challenger;
    }

    /// @inheritdoc IMultiProofGame
    function proofBitmap() external view returns (uint8) {
        return claimData.proofBitmap;
    }

    /// @inheritdoc IMultiProofGame
    function invalidationReason() external view returns (ProofLib.InvalidationReason) {
        return claimData.invalidationReason;
    }

    /// @inheritdoc IMultiProofGame
    function challengeDeadline() public view returns (Timestamp) {
        return Timestamp.wrap(createdAt.raw() + challengePeriod.raw());
    }

    /// @inheritdoc IMultiProofGame
    function proofDeadline() public view returns (Timestamp) {
        return Timestamp.wrap(createdAt.raw() + proofPeriod.raw());
    }

    /// @inheritdoc IMultiProofGame
    function state() public view returns (ProofLib.RootState) {
        if (status == GameStatus.DEFENDER_WINS) return ProofLib.RootState.FINALIZED;
        if (status == GameStatus.CHALLENGER_WINS) return ProofLib.RootState.INVALIDATED;
        return claimData.challenger == address(0) ? ProofLib.RootState.PROPOSED : ProofLib.RootState.CHALLENGED;
    }

    /// @notice Returns the verifier configured for `lane`.
    function _verifierFor(ProofLib.ProofLane lane) internal view returns (IWorldChainProofVerifier) {
        if (lane == ProofLib.ProofLane.VALIDITY_PROOF) return validityProofVerifier;
        if (lane == ProofLib.ProofLane.TEE_ATTESTATION) return teeVerifier;
        return securityCouncil;
    }
}
