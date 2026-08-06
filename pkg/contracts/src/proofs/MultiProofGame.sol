// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {LibProof} from "./lib/LibProof.sol";
import {GameTypes} from "./GameTypes.sol";
import {IMultiProofGame} from "./interfaces/IMultiProofGame.sol";
import {IWorldChainProofVerifier} from "./interfaces/IWorldChainProofVerifier.sol";

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
import {ISystemConfig} from "@optimism-bedrock/interfaces/L1/ISystemConfig.sol";
import {ISemver} from "@optimism-bedrock/interfaces/universal/ISemver.sol";

/// @title MultiProofGame
/// @author World Contributors
/// @notice A Multi Proof `IDisputeGame` supporing 3 different proof 'lanes'.
///     1.) ZK Validity Proof via Succinct Prover
///     2.) AWS Nitro Enclave Attestation
///     3.) Security Council Attestations
/// @dev Additional Proof Lanes may be added in the future for this game type.
/// @custom:security-contact security@toolsforhumanity.com
contract MultiProofGame is Clone, ISemver, IMultiProofGame {
    using LibProof for LibProof.ProofLane;
    using LibProof for uint8;

    ////////////////////////////////////////////////////////////////
    //                      STATE VARIABLES                       //
    ////////////////////////////////////////////////////////////////

    /// @notice Semantic version.
    /// @custom:semver 1.0.0
    string public constant version = "1.0.0";

    /// @notice Share of a forfeited proposer bond paid to a winning challenger, in basis points.
    uint256 public constant CHALLENGER_REWARD_BPS = 5_000;

    /// @notice Basis-point denominator for `CHALLENGER_REWARD_BPS`.
    uint256 internal constant BPS = 10_000;

    /// @notice Total number of proof lanes defined by the protocol.
    uint8 public constant PROOF_LANE_COUNT = LibProof.PROOF_LANE_COUNT;

    /// @notice Number of distinct proof lanes required to finalize a challenged root.
    uint8 public immutable PROOF_THRESHOLD;
    /// @notice Commitment binding this deployment to its chain
    bytes32 public immutable domainHash;

    /// @notice Hash of the rollup configuration the proof lanes verify against.
    bytes32 public immutable rollupConfigHash;

    /// @notice Seconds a proposal has to land its first proof lane, and the length of the
    ///         challenge window that opens once it does.
    Duration public immutable challengePeriod;

    /// @notice Seconds after creation a challenged proposal has to reach the proof threshold.
    Duration public immutable proofPeriod;

    /// @notice Bond required to create a proposal.
    uint256 public immutable proposerBond;

    /// @notice Bond required to challenge a proposal.
    uint256 public immutable challengerBond;

    /// @notice Recipient of the share of forfeited proposer bonds not paid to a challenger.
    address public immutable protocolFeeRecipient;

    /// @notice Verifiers backing the proof lanes, indexed by `LibProof.ProofLane`.
    IWorldChainProofVerifier public immutable validityProofVerifier;
    IWorldChainProofVerifier public immutable teeVerifier;
    IWorldChainProofVerifier public immutable securityCouncil;

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

    /// @notice Credited balances for refund recipients.
    mapping(address recipient => uint256 amount) public refundModeCredit;

    /// @notice The bond distribution mode of the game.
    BondDistributionMode public bondDistributionMode;

    /// @notice A boolean for whether or not the game type was respected when the game was created.
    bool public wasRespectedGameTypeWhenCreated;

    /// @notice The total bonds deposited into the game.
    uint256 public totalBonds;

    ////////////////////////////////////////////////////////////////
    //                        Constructor                         //
    ////////////////////////////////////////////////////////////////

    constructor(GameConfig memory config) {
        if (
            config.challengePeriod == 0 || config.proofPeriod <= config.challengePeriod || config.proposerBond == 0
                || config.challengerBond == 0 || config.proofThreshold == 0
                || config.proofThreshold > LibProof.PROOF_LANE_COUNT || config.protocolFeeRecipient == address(0)
                || address(config.anchorStateRegistry) == address(0) || address(config.weth) == address(0)
                || address(config.validityProofVerifier) == address(0) || address(config.teeVerifier) == address(0)
                || address(config.securityCouncil) == address(0)
        ) {
            revert InvalidActivationParameters();
        }

        // The factory and chain ID are read from the registry rather than configured, so they
        // cannot disagree with it.
        IDisputeGameFactory factory = config.anchorStateRegistry.disputeGameFactory();
        ISystemConfig systemConfig = config.anchorStateRegistry.systemConfig();
        uint256 chainId = systemConfig.l2ChainId();
        if (address(factory) == address(0) || chainId == 0) {
            revert InvalidActivationParameters();
        }
        if (address(config.weth.systemConfig()) != address(systemConfig)) {
            revert InconsistentSystemConfiguration();
        }

        domainHash = LibProof.domainHash(chainId, LibProof.PROOF_SYSTEM_VERSION, config.rollupConfigHash);
        rollupConfigHash = config.rollupConfigHash;
        challengePeriod = Duration.wrap(config.challengePeriod);
        proofPeriod = Duration.wrap(config.proofPeriod);
        proposerBond = config.proposerBond;
        challengerBond = config.challengerBond;
        protocolFeeRecipient = config.protocolFeeRecipient;
        PROOF_THRESHOLD = config.proofThreshold;
        validityProofVerifier = config.validityProofVerifier;
        teeVerifier = config.teeVerifier;
        securityCouncil = config.securityCouncil;
        disputeGameFactory = factory;
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

    /// @notice Getter for the root claim for a given L2 chain ID.
    /// @return rootClaim_ The root claim of the DisputeGame.
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
        if (msg.sender != address(disputeGameFactory)) {
            revert NotDisputeGameFactory(msg.sender);
        }

        // Defense-in-depth against `setInitBond` drifting from the configured proposer bond.
        if (msg.value != proposerBond) revert IncorrectBondAmount();

        // Preserves the former propose-time registry pause gate.
        if (anchorStateRegistry.paused()) revert GamePaused();

        // INVARIANT: The proposal must target the domain this implementation proves against.
        bytes32 proposalDomainHash_ = proposalDomainHash();
        if (proposalDomainHash_ != domainHash) {
            revert InvalidDomainHash(domainHash, proposalDomainHash_);
        }

        address parentRef_ = parentRef();
        uint256 l2SequenceNumber_ = l2SequenceNumber();
        Claim rootClaim_ = rootClaim();
        uint256 attempt_ = attempt();

        // INVARIANT: The parent must be a registered, respected, live game of this type.
        if (parentRef_ != address(anchorStateRegistry) && !_isValidGame(IDisputeGame(parentRef_))) {
            revert InvalidParentGame();
        }

        // INVARIANT: Each proposal must advance its parent. Cadence beyond that is proposer
        // policy: the range is fully committed by `rootId`, and proof cost scales with it.
        (, uint256 startingBlockNumber_) = startingProposal();
        if (l2SequenceNumber_ <= startingBlockNumber_) {
            revert InvalidL2BlockNumber(startingBlockNumber_, l2SequenceNumber_);
        }

        // Per spec, the sequence number must fit within a uint64.
        if (l2SequenceNumber_ > type(uint64).max) {
            revert UnexpectedRootClaim(rootClaim_);
        }

        // Retries: attempt N is only proposable when attempt N-1 for the identical transition
        // timed out on proofs or was created before WIP-1006 became respected. The latter
        // prevents a pre-cutover game from permanently occupying the factory UUID. Inherited
        // invalidations rebase onto a replacement parent and therefore restart at attempt zero.
        // Duplicate attempts are impossible: the factory UUID covers (gameType, rootClaim,
        // extraData).
        if (attempt_ > 0) {
            bytes memory previousExtraData = abi.encode(domainHash, l2SequenceNumber_, parentRef_, attempt_ - 1);
            (IDisputeGame previous,) =
                disputeGameFactory.games(GameTypes.MULTI_PROOF_GAME_TYPE, rootClaim_, previousExtraData);
            if (
                address(previous) == address(0)
                    || (previous.wasRespectedGameTypeWhenCreated()
                        && (previous.status() != GameStatus.CHALLENGER_WINS
                            || IMultiProofGame(address(previous)).invalidationReason()
                                != LibProof.InvalidationReason.PROOF_TIMEOUT))
            ) {
                revert GameNotRetryable(keccak256(
                        abi.encode(GameTypes.MULTI_PROOF_GAME_TYPE, rootClaim_, previousExtraData)
                    ));
            }
        }

        // `initialize` runs in the same transaction as `DisputeGameFactory.create`, which set
        // `l1Head = blockhash(block.number - 1)`.
        _l1OriginNumber = uint64(block.number - 1);

        // Set the root claim.
        claimData = ClaimData({
            status: ProposalStatus.Unchallenged,
            challenger: address(0),
            deadline: Timestamp.wrap(uint64(block.timestamp + challengePeriod.raw())),
            proofBitmap: 0,
            invalidationReason: LibProof.InvalidationReason.NONE
        });

        // Set the game as initialized.
        initialized = true;

        // Deposit the bond into DelayedWETH and track refund credit.
        refundModeCredit[gameCreator()] += msg.value;
        totalBonds += msg.value;
        weth.deposit{value: msg.value}();

        // Set the game's starting timestamp.
        createdAt = Timestamp.wrap(uint64(block.timestamp));

        // Set whether the game type was respected when the game was created.
        wasRespectedGameTypeWhenCreated =
            anchorStateRegistry.respectedGameType().raw() == GameTypes.MULTI_PROOF_GAME_TYPE.raw();

        emit WorldChainGameCreated(
            rootId(),
            parentRef_,
            Claim.unwrap(rootClaim_),
            l2SequenceNumber_,
            Hash.unwrap(l1Head()),
            _l1OriginNumber,
            attempt_,
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

        // INVARIANT: Only a proven claim can be disputed.
        if (claimData.proofBitmap == 0) revert NoProofToChallenge();

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

        // Deposit the bond into DelayedWETH and track refund credit.
        refundModeCredit[msg.sender] += msg.value;
        totalBonds += msg.value;
        weth.deposit{value: msg.value}();

        emit Challenged(msg.sender, claimData.deadline.raw());

        return claimData.status;
    }

    /// @notice Proves the game through one of the proof lanes.
    /// @param laneId The `LibProof.ProofLane` being submitted.
    /// @param proof The lane-specific proof bytes binding `rootId`.
    function submitProofLane(uint8 laneId, bytes calldata proof) external returns (ProposalStatus) {
        // INVARIANT: Cannot prove if the game is already resolved.
        if (status != GameStatus.IN_PROGRESS) revert ClaimAlreadyResolved();

        // INVARIANT: Cannot prove if the parent game is invalid: this game will resolve
        // `CHALLENGER_WINS` with `INVALID_PARENT` regardless of its own proof state.
        (GameStatus parentStatus, bool parentBlacklisted) = _parentResolution();
        if (parentBlacklisted || parentStatus == GameStatus.CHALLENGER_WINS) {
            revert InvalidParentGame();
        }

        // INVARIANT: Cannot prove if the game is over. Also caps the bitmap at the threshold:
        // once enough lanes have been accepted, further submissions are rejected.
        if (gameOver()) revert GameOver();

        if (laneId >= PROOF_LANE_COUNT) revert InvalidLane(laneId);
        LibProof.ProofLane lane = LibProof.ProofLane(laneId);
        uint8 mask = lane.laneMask();
        bytes32 rootId_ = rootId();

        // No-op on resubmission so racing provers do not revert each other.
        if (claimData.proofBitmap & mask != 0) {
            emit DuplicateProofLane(lane, rootId_, claimData.proofBitmap);
            return claimData.status;
        }

        // Verify the proof against the verifier configured for this lane.
        IWorldChainProofVerifier verifier = lane.verifierFor(validityProofVerifier, teeVerifier, securityCouncil);
        if (!verifier.verify(rootId_, _transition(), proof)) {
            revert InvalidProof(lane, rootId_);
        }

        // The first lane opens the challenge window; challengers get a full period from it.
        if (claimData.proofBitmap == 0) {
            claimData.deadline = Timestamp.wrap(uint64(block.timestamp) + challengePeriod.raw());
        }

        claimData.proofBitmap |= mask;

        // Update the status of the proposal. Unchallenged, a single lane is a valid proof that
        // finalizes once the challenge window closes; challenged, only the threshold is.
        if (claimData.proofBitmap.hasThreshold(PROOF_THRESHOLD)) {
            claimData.status = claimData.challenger == address(0)
                ? ProposalStatus.UnchallengedAndValidProofProvided
                : ProposalStatus.ChallengedAndValidProofProvided;
        } else if (claimData.challenger == address(0)) {
            claimData.status = ProposalStatus.UnchallengedAndValidProofProvided;
        }

        emit ProofSubmitted(lane, rootId_, claimData.proofBitmap);

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
            // An invalid parent invalidates this game regardless of its own proof state.
            status = GameStatus.CHALLENGER_WINS;
            claimData.invalidationReason = LibProof.InvalidationReason.INVALID_PARENT;
            normalModeCredit[gameCreator()] += proposerBond;
            if (claimData.challenger != address(0)) {
                normalModeCredit[claimData.challenger] += challengerBond;
            }
        } else if (parentStatus == GameStatus.IN_PROGRESS) {
            // INVARIANT: Cannot resolve a game if the parent game has not been resolved.
            revert ParentGameNotResolved();
        } else {
            // INVARIANT: Game must be completed either by clock expiration or the threshold.
            if (!gameOver()) revert GameNotOver();

            // Determine status based on claim status.
            if (
                claimData.status == ProposalStatus.UnchallengedAndValidProofProvided
                    || claimData.status == ProposalStatus.ChallengedAndValidProofProvided
            ) {
                status = GameStatus.DEFENDER_WINS;
                normalModeCredit[gameCreator()] += totalBonds;
            } else if (claimData.status == ProposalStatus.Unchallenged || claimData.status == ProposalStatus.Challenged)
            {
                // An applicable proof window expired below its requirement. A proofless
                // proposal cannot win merely because nobody challenged it.
                //
                // Note: The challenger is paid back less than the pair staked, so a self-challenge cannot break even.
                status = GameStatus.CHALLENGER_WINS;
                claimData.invalidationReason = LibProof.InvalidationReason.PROOF_TIMEOUT;
                uint256 challengerCredit = claimData.challenger == address(0)
                    ? 0
                    : challengerBond + (proposerBond * CHALLENGER_REWARD_BPS) / BPS;
                if (challengerCredit != 0) {
                    normalModeCredit[claimData.challenger] += challengerCredit;
                }
                normalModeCredit[protocolFeeRecipient] += totalBonds - challengerCredit;
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
        returns (bool resolvable, GameStatus outcome, LibProof.InvalidationReason reason)
    {
        if (status != GameStatus.IN_PROGRESS) {
            return (false, status, claimData.invalidationReason);
        }

        (GameStatus parentStatus, bool parentBlacklisted) = _parentResolution();
        if (parentBlacklisted || parentStatus == GameStatus.CHALLENGER_WINS) {
            return (true, GameStatus.CHALLENGER_WINS, LibProof.InvalidationReason.INVALID_PARENT);
        }
        if (parentStatus == GameStatus.IN_PROGRESS || !gameOver()) {
            return (false, GameStatus.IN_PROGRESS, LibProof.InvalidationReason.NONE);
        }

        bool proven = claimData.status == ProposalStatus.UnchallengedAndValidProofProvided
            || claimData.status == ProposalStatus.ChallengedAndValidProofProvided;
        return proven
            ? (true, GameStatus.DEFENDER_WINS, LibProof.InvalidationReason.NONE)
            : (true, GameStatus.CHALLENGER_WINS, LibProof.InvalidationReason.PROOF_TIMEOUT);
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

        // Zero the credit and unlock it in DelayedWETH.
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
        if (!anchorStateRegistry.isGameFinalized(self)) {
            revert GameNotFinalized();
        }

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
        gameOver_ =
            claimData.deadline.raw() <= uint64(block.timestamp) || claimData.proofBitmap.hasThreshold(PROOF_THRESHOLD);
    }

    /// @inheritdoc IMultiProofGame
    function credit(address recipient) external view returns (uint256 credit_) {
        if (bondDistributionMode == BondDistributionMode.REFUND) {
            credit_ = refundModeCredit[recipient];
        } else {
            credit_ = normalModeCredit[recipient];
        }
    }

    /// @inheritdoc IMultiProofGame
    /// @dev Derived, not stored: the preimage is fixed at creation, so recomputing keeps a
    ///      single source of truth.
    function rootId() public view returns (bytes32) {
        return LibProof.rootId(
            domainHash,
            parentRef(),
            Claim.unwrap(rootClaim()),
            l2SequenceNumber(),
            Hash.unwrap(l1Head()),
            _l1OriginNumber
        );
    }

    /// @inheritdoc IMultiProofGame
    function startingProposal() public view returns (Hash root_, uint256 l2SequenceNumber_) {
        address parentRef_ = parentRef();
        if (parentRef_ == address(anchorStateRegistry)) {
            // When there is no parent game, the starting output root is the starting anchor.
            Proposal memory anchor = anchorStateRegistry.getStartingAnchorRoot();
            return (anchor.root, anchor.l2SequenceNumber);
        }
        IDisputeGame parent = IDisputeGame(parentRef_);
        return (Hash.wrap(parent.rootClaim().raw()), parent.l2SequenceNumber());
    }

    /// @notice Only the starting block number of the game.
    function startingBlockNumber() external view returns (uint256 startingBlockNumber_) {
        (, startingBlockNumber_) = startingProposal();
    }

    /// @notice Starting output root of the game.
    function startingRootHash() external view returns (Hash startingRootHash_) {
        (startingRootHash_,) = startingProposal();
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
    function invalidationReason() external view returns (LibProof.InvalidationReason) {
        return claimData.invalidationReason;
    }

    /// @inheritdoc IMultiProofGame
    function challengeDeadline() public view returns (Timestamp) {
        return claimData.deadline;
    }

    /// @inheritdoc IMultiProofGame
    function proofDeadline() public view returns (Timestamp) {
        return Timestamp.wrap(createdAt.raw() + proofPeriod.raw());
    }

    /// @notice The transition public values a proof for this game must attest.
    function _transition() internal view returns (LibProof.TransitionPublicValues memory) {
        (Hash startingRootHash_, uint256 startingBlockNumber_) = startingProposal();
        return LibProof.TransitionPublicValues({
            l1Head: Hash.unwrap(l1Head()),
            l2PreRoot: Hash.unwrap(startingRootHash_),
            // forge-lint: disable-next-line(unsafe-typecast)
            l2PreBlockNumber: uint64(startingBlockNumber_),
            l2PostRoot: Claim.unwrap(rootClaim()),
            // forge-lint: disable-next-line(unsafe-typecast)
            l2PostBlockNumber: uint64(l2SequenceNumber()),
            rollupConfigHash: rollupConfigHash
        });
    }
}
