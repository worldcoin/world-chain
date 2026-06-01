// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {CWIACloneReader} from "./external/CWIACloneReader.sol";
import {IInitializable} from "./external/IInitializable.sol";
import {IDisputeGame} from "./external/IDisputeGame.sol";
import {IDelayedWETH} from "./external/IDelayedWETH.sol";
import {Claim, GameStatus, GameType, Hash, Timestamp} from "./external/Types.sol";
import {WorldChainProofLib} from "./WorldChainProofLib.sol";
import {IWorldChainProofVerifier} from "./verifiers/IWorldChainProofVerifier.sol";
import {IWorldChainStakingRegistry} from "./interfaces/IWorldChainStakingRegistry.sol";

/// @title WorldChainProofSystemGame
/// @notice WIP-1006 proof-system game, deployed as a canonical OP Stack
///         `IDisputeGame` and instantiated by the deployed `DisputeGameFactory`
///         via `create(gameType, rootClaim, extraData)` (the Base "Azul"
///         integration model). The factory clones this implementation and calls
///         `initialize{value: initBond}()`.
/// @dev Per-proposal data is read from the CWIA clone calldata; domain and
///      verifier/staking/escrow configuration is baked into the implementation
///      as immutables. Optimistic resolution: an unchallenged root resolves
///      `DEFENDER_WINS` after `CHALLENGE_PERIOD` with zero proofs; a challenged
///      root resolves `DEFENDER_WINS` only when ≥ `PROOF_THRESHOLD` of the 3
///      lanes support it before `proofDeadline`, else `invalidate()` resolves
///      `CHALLENGER_WINS`.
contract WorldChainProofSystemGame is IDisputeGame, CWIACloneReader {
    using WorldChainProofLib for WorldChainProofLib.ProofLane;

    /// @notice CWIA offsets (relative to the immutable-args region) appended by
    ///         the OP `DisputeGameFactory`:
    ///         `[0,20) creator`, `[20,52) rootClaim`, `[52,84) l1Head`,
    ///         `[84,...) extraData`.
    uint256 internal constant CWIA_CREATOR_OFFSET = 0x00;
    uint256 internal constant CWIA_ROOT_CLAIM_OFFSET = 0x14;
    uint256 internal constant CWIA_L1_HEAD_OFFSET = 0x34;
    uint256 internal constant CWIA_EXTRA_DATA_OFFSET = 0x54;

    /// @notice WIP-1006 protocol constants.
    uint8 public constant PROOF_THRESHOLD = WorldChainProofLib.PROOF_THRESHOLD;
    uint8 public constant PROOF_LANE_COUNT = WorldChainProofLib.PROOF_LANE_COUNT;

    // ----------------------------- Immutables ------------------------------ //

    /// @notice The OP game type assigned to this implementation.
    GameType internal immutable GAME_TYPE;

    /// @notice Verifier-immutable domain hash committed into `rootId`.
    bytes32 public immutable domainHash;

    /// @notice Activation parameters (immutable per WIP-1006).
    uint64 public immutable challengePeriod;
    uint64 public immutable proofPeriod;

    /// @notice Lane verifiers, indexed by `WorldChainProofLib.ProofLane`.
    IWorldChainProofVerifier public immutable validityProofVerifier;
    IWorldChainProofVerifier public immutable teeVerifier;
    IWorldChainProofVerifier public immutable securityCouncil;

    /// @notice World Chain challenger staking/escrow registry.
    IWorldChainStakingRegistry public immutable stakingRegistry;

    /// @notice DelayedWETH escrow for the proposer bond.
    IDelayedWETH public immutable weth;

    // Domain components retained so the game can re-validate `extraData`.
    uint256 internal immutable chainId;
    uint256 internal immutable proofSystemVersion;
    bytes32 internal immutable rollupConfigHash;
    uint256 internal immutable blockInterval;
    uint256 internal immutable intermediateBlockInterval;

    // ------------------------------- Storage ------------------------------- //

    /// @notice Whether `initialize()` has run.
    bool public initialized;

    /// @inheritdoc IDisputeGame
    bool public wasRespectedGameTypeWhenCreated;

    /// @inheritdoc IDisputeGame
    Timestamp public createdAt;

    /// @inheritdoc IDisputeGame
    Timestamp public resolvedAt;

    /// @notice Current OP game status (mapped from the WIP state machine).
    GameStatus internal gameStatus;

    /// @notice Parsed L2 block number for the proposed root.
    uint256 public l2BlockNumberValue;

    /// @notice Parsed parent reference (anchor registry or parent game).
    address public parentRef;

    /// @notice Commitment over the ordered intermediate roots in `extraData`.
    bytes32 public intermediateRootsHash;

    /// @notice Factory-captured L1 origin number, pinned at `initialize()`.
    uint256 public l1OriginNumber;

    /// @notice The WIP-1006 proof-bound commitment every lane MUST bind to.
    bytes32 public rootId;

    /// @notice Proposer bond deposited into DelayedWETH at init.
    uint256 public proposerBond;

    /// @notice WIP timestamps.
    uint64 public challengeDeadline;
    uint64 public challengedAt;
    uint64 public proofDeadline;

    /// @notice Per-root lane support bitmap.
    uint8 public proofBitmap;

    /// @notice Recorded challengers (for accounting and bond release).
    address[] public challengers;
    mapping(address challenger => bool recorded) public hasChallenged;

    // ------------------------------- Errors -------------------------------- //

    error AlreadyInitialized();
    error AnchorRootNotFound();
    error GameNotInProgress();
    error GameNotResolved();
    error ChallengePeriodElapsed(uint256 timestamp, uint256 challengeDeadline);
    error ChallengePeriodOpen(uint256 timestamp, uint256 challengeDeadline);
    error ProofPeriodElapsed(uint256 timestamp, uint256 proofDeadline);
    error ProofPeriodOpen(uint256 timestamp, uint256 proofDeadline);
    error NotChallenged();
    error DuplicateChallenge(address challenger);
    error InvalidLane(uint8 lane);
    error InvalidProof(WorldChainProofLib.ProofLane lane, bytes32 rootId);
    error ThresholdNotMet();
    error ThresholdAlreadyMet();
    error TransferFailed(address recipient, uint256 amount);

    // ------------------------------- Events -------------------------------- //

    event Challenged(address indexed challenger, uint64 proofDeadline);
    event ProofLaneSupported(WorldChainProofLib.ProofLane indexed lane, bytes32 indexed rootId, uint8 proofBitmap);
    event DuplicateProofLane(WorldChainProofLib.ProofLane indexed lane, bytes32 indexed rootId, uint8 proofBitmap);

    /// @param gameType_ The OP game type assigned to this implementation.
    /// @param domain_ Verifier-immutable domain constants.
    /// @param challengePeriod_ WIP `CHALLENGE_PERIOD`.
    /// @param proofPeriod_ WIP `PROOF_PERIOD`.
    /// @param validityProofVerifier_ `VALIDITY_PROOF` lane verifier.
    /// @param teeVerifier_ `TEE_ATTESTATION` lane verifier.
    /// @param securityCouncil_ `SECURITY_COUNCIL` lane verifier.
    /// @param stakingRegistry_ World Chain challenger registry.
    /// @param weth_ DelayedWETH escrow for the proposer bond.
    constructor(
        GameType gameType_,
        WorldChainProofLib.Domain memory domain_,
        uint64 challengePeriod_,
        uint64 proofPeriod_,
        IWorldChainProofVerifier validityProofVerifier_,
        IWorldChainProofVerifier teeVerifier_,
        IWorldChainProofVerifier securityCouncil_,
        IWorldChainStakingRegistry stakingRegistry_,
        IDelayedWETH weth_
    ) {
        GAME_TYPE = gameType_;
        domainHash = WorldChainProofLib.domainHash(domain_);
        chainId = domain_.chainId;
        proofSystemVersion = domain_.proofSystemVersion;
        rollupConfigHash = domain_.rollupConfigHash;
        blockInterval = domain_.blockInterval;
        intermediateBlockInterval = domain_.intermediateBlockInterval;
        challengePeriod = challengePeriod_;
        proofPeriod = proofPeriod_;
        validityProofVerifier = validityProofVerifier_;
        teeVerifier = teeVerifier_;
        securityCouncil = securityCouncil_;
        stakingRegistry = stakingRegistry_;
        weth = weth_;
    }

    // ---------------------------- Initialization --------------------------- //

    /// @inheritdoc IInitializable
    /// @dev Called by the `DisputeGameFactory` immediately after cloning. Parses
    ///      and validates the CWIA `extraData`, pins the L1 origin, computes
    ///      `rootId`, deposits the proposer bond into DelayedWETH, and starts the
    ///      challenge window.
    function initialize() external payable {
        if (initialized) revert AlreadyInitialized();
        initialized = true;

        bytes32 rootClaim_ = Claim.unwrap(rootClaim());
        bytes memory extra = extraData();

        WorldChainProofLib.Domain memory domain = WorldChainProofLib.Domain({
            chainId: chainId,
            proofSystemVersion: proofSystemVersion,
            rollupConfigHash: rollupConfigHash,
            blockInterval: blockInterval,
            intermediateBlockInterval: intermediateBlockInterval
        });

        (uint256 l2Block, address parent, bytes32 rootsHash) =
            WorldChainProofLib.validateAndParseExtraData(extra, domain, rootClaim_);

        l2BlockNumberValue = l2Block;
        parentRef = parent;
        intermediateRootsHash = rootsHash;

        // The OP factory pins `l1Head` (parent blockhash) into CWIA args; the
        // paired L1 origin number is captured here, in the creation block.
        l1OriginNumber = block.number == 0 ? 0 : block.number - 1;

        rootId = WorldChainProofLib.rootId(
            domainHash, parent, rootClaim_, l2Block, rootsHash, Hash.unwrap(l1Head()), l1OriginNumber
        );

        proposerBond = msg.value;
        createdAt = Timestamp.wrap(uint64(block.timestamp));
        challengeDeadline = uint64(block.timestamp) + challengePeriod;
        gameStatus = GameStatus.IN_PROGRESS;

        // Escrow the proposer bond in DelayedWETH (mirrors Base).
        weth.deposit{value: msg.value}();
    }

    // ------------------------------ Challenge ------------------------------ //

    /// @notice Challenges the proposed root. Optimistic: no proof of invalidity
    ///         is required. Shifts the burden of proof to the proposer.
    /// @dev MUST be staked, before `challengeDeadline`, and lock `CHALLENGER_BOND`
    ///      (forwarded to the staking registry). The first challenge opens the
    ///      proof window; subsequent challenges do not extend it.
    function challenge() external payable {
        if (gameStatus != GameStatus.IN_PROGRESS) revert GameNotInProgress();
        if (block.timestamp >= challengeDeadline) {
            revert ChallengePeriodElapsed(block.timestamp, challengeDeadline);
        }
        if (hasChallenged[msg.sender]) revert DuplicateChallenge(msg.sender);

        hasChallenged[msg.sender] = true;
        challengers.push(msg.sender);

        // Locks the bond; reverts if the caller is not staked or the bond is wrong.
        stakingRegistry.lockChallengerBond{value: msg.value}(address(this), msg.sender);

        if (challengedAt == 0) {
            challengedAt = uint64(block.timestamp);
            proofDeadline = uint64(block.timestamp) + proofPeriod;
        }

        emit Challenged(msg.sender, proofDeadline);
    }

    /// @notice Returns whether the game has been challenged.
    function challenged() public view returns (bool) {
        return challengedAt != 0;
    }

    // --------------------------- Proposer defense -------------------------- //

    /// @notice Submits lane support for the challenged root. Permissionless.
    /// @dev Verifies the root is challenged, the proof window is open, the
    ///      material binds to `rootId`, and the lane verifier accepts. Sets the
    ///      lane bit; finalizes when the threshold is met. Duplicate lane
    ///      submissions are a no-op (do not increase the count).
    /// @param laneId The `WorldChainProofLib.ProofLane` index.
    /// @param proof Lane-specific opaque proof material.
    function submitProofLane(uint8 laneId, bytes calldata proof) external {
        if (gameStatus != GameStatus.IN_PROGRESS) revert GameNotInProgress();
        if (!challenged()) revert NotChallenged();
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

        if (WorldChainProofLib.hasThreshold(proofBitmap)) {
            _resolveDefenderWins();
        }
    }

    // ----------------------------- Resolution ------------------------------ //

    /// @inheritdoc IDisputeGame
    /// @dev Resolves an unchallenged root to `DEFENDER_WINS` after
    ///      `CHALLENGE_PERIOD`, or a challenged-and-defended root to
    ///      `DEFENDER_WINS` once the threshold is met. Challenged-but-undefended
    ///      roots are resolved via `invalidate()`.
    function resolve() external returns (GameStatus status_) {
        if (gameStatus != GameStatus.IN_PROGRESS) revert GameNotInProgress();

        if (!challenged()) {
            if (block.timestamp < challengeDeadline) {
                revert ChallengePeriodOpen(block.timestamp, challengeDeadline);
            }
            return _resolveDefenderWins();
        }

        if (!WorldChainProofLib.hasThreshold(proofBitmap)) revert ThresholdNotMet();
        return _resolveDefenderWins();
    }

    /// @notice Invalidates a challenged root the proposer failed to defend to
    ///         `PROOF_THRESHOLD` lanes by `proofDeadline`. Resolves
    ///         `CHALLENGER_WINS`. Permissionless.
    function invalidate() external returns (GameStatus status_) {
        if (gameStatus != GameStatus.IN_PROGRESS) revert GameNotInProgress();
        if (!challenged()) revert NotChallenged();
        if (block.timestamp < proofDeadline) {
            revert ProofPeriodOpen(block.timestamp, proofDeadline);
        }
        if (WorldChainProofLib.hasThreshold(proofBitmap)) revert ThresholdAlreadyMet();

        gameStatus = GameStatus.CHALLENGER_WINS;
        resolvedAt = Timestamp.wrap(uint64(block.timestamp));

        // Refund every challenger's bond; forfeit the proposer bond to the
        // first (successful) challenger via DelayedWETH.
        uint256 count = challengers.length;
        for (uint256 i = 0; i < count; i++) {
            stakingRegistry.refundChallengerBond(address(this), challengers[i]);
        }

        // Proposer bond -> successful challenger (the first recorded challenger).
        weth.unlock(address(this), proposerBond);

        emit Resolved(GameStatus.CHALLENGER_WINS);
        return GameStatus.CHALLENGER_WINS;
    }

    /// @notice Two-phase bond payout. After resolution, anyone may unlock the
    ///         resolved bond (done in `resolve`/`invalidate`); after the
    ///         DelayedWETH delay elapses, the eligible recipient claims it.
    /// @dev Defender-wins -> the proposer (game creator); challenger-wins -> the
    ///      first (successful) challenger. After `resolve`/`invalidate` has
    ///      unlocked the bond and the DelayedWETH delay has elapsed, anyone may
    ///      trigger the claim, but the proceeds are routed to the protocol-defined
    ///      winner only.
    function claimCredit() external {
        if (gameStatus == GameStatus.IN_PROGRESS) revert GameNotResolved();
        if (proposerBond == 0) return;
        uint256 amount = proposerBond;
        proposerBond = 0;

        address payable recipient =
            gameStatus == GameStatus.DEFENDER_WINS ? payable(gameCreator()) : payable(challengers[0]);

        // Second phase: withdraw from DelayedWETH (only succeeds after the delay).
        weth.withdraw(address(this), amount);
        _transfer(recipient, amount);
    }

    /// @dev Resolves to `DEFENDER_WINS`, refunding/escrowing bonds accordingly:
    ///      proposer bond unlocked back to the proposer, challenger bonds
    ///      forfeited.
    function _resolveDefenderWins() internal returns (GameStatus) {
        gameStatus = GameStatus.DEFENDER_WINS;
        resolvedAt = Timestamp.wrap(uint64(block.timestamp));

        uint256 count = challengers.length;
        for (uint256 i = 0; i < count; i++) {
            stakingRegistry.forfeitChallengerBond(address(this), challengers[i]);
        }

        weth.unlock(address(this), proposerBond);

        emit Resolved(GameStatus.DEFENDER_WINS);
        return GameStatus.DEFENDER_WINS;
    }

    /// @notice Accepts ETH from DelayedWETH on withdrawal.
    receive() external payable {}

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

    // --------------------------- IDisputeGame views ------------------------ //

    /// @inheritdoc IDisputeGame
    function status() external view returns (GameStatus) {
        return gameStatus;
    }

    /// @inheritdoc IDisputeGame
    function gameType() public view returns (GameType gameType_) {
        gameType_ = GAME_TYPE;
    }

    /// @inheritdoc IDisputeGame
    function gameCreator() public pure returns (address creator_) {
        creator_ = _getArgAddress(CWIA_CREATOR_OFFSET);
    }

    /// @inheritdoc IDisputeGame
    function rootClaim() public pure returns (Claim rootClaim_) {
        rootClaim_ = Claim.wrap(_getArgBytes32(CWIA_ROOT_CLAIM_OFFSET));
    }

    /// @inheritdoc IDisputeGame
    function l1Head() public pure returns (Hash l1Head_) {
        l1Head_ = Hash.wrap(_getArgBytes32(CWIA_L1_HEAD_OFFSET));
    }

    /// @inheritdoc IDisputeGame
    /// @dev The L2 block number is the first word of `extraData`, so it is read
    ///      directly from the CWIA args (matching the OP `FaultDisputeGame`
    ///      idiom of `_getArgUint256(0x54)`). Equals the value `initialize()`
    ///      validated and stored in `l2BlockNumberValue`.
    function l2BlockNumber() public pure returns (uint256 l2BlockNumber_) {
        l2BlockNumber_ = _getArgUint256(CWIA_EXTRA_DATA_OFFSET);
    }

    /// @inheritdoc IDisputeGame
    function extraData() public pure returns (bytes memory extraData_) {
        uint256 offset = _getImmutableArgsOffset();
        uint256 total;
        assembly {
            total := sub(calldatasize(), add(offset, 2))
        }
        extraData_ = _getArgBytes(CWIA_EXTRA_DATA_OFFSET, total - CWIA_EXTRA_DATA_OFFSET);
    }

    /// @inheritdoc IDisputeGame
    function gameData() external view returns (GameType gameType_, Claim rootClaim_, bytes memory extraData_) {
        gameType_ = gameType();
        rootClaim_ = rootClaim();
        extraData_ = extraData();
    }

    /// @notice Distinct lane count currently supporting the root.
    function proofCount() external view returns (uint8) {
        return WorldChainProofLib.proofCount(proofBitmap);
    }
}
