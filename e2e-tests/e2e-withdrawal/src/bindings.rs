use alloy_primitives::{Address, B256};
use alloy_sol_types::sol;
use serde::{Deserialize, Serialize};

sol! {
    #[derive(Debug, Serialize, Deserialize)]
    struct WithdrawalTransaction {
        uint256 nonce;
        address sender;
        address target;
        uint256 value;
        uint256 gasLimit;
        bytes data;
    }

    struct OutputRootProof {
        bytes32 version;
        bytes32 stateRoot;
        bytes32 messagePasserStorageRoot;
        bytes32 latestBlockhash;
    }

    #[sol(rpc)]
    interface L2ToL1MessagePasser {
        event MessagePassed(
            uint256 indexed nonce,
            address indexed sender,
            address indexed target,
            uint256 value,
            uint256 gasLimit,
            bytes data,
            bytes32 withdrawalHash
        );

        function initiateWithdrawal(address target, uint256 gasLimit, bytes data) external payable;
    }

    #[sol(rpc)]
    interface OptimismPortal {
        function anchorStateRegistry() external view returns (address);
        function disputeGameFactory() external view returns (address);
        function disputeGameFinalityDelaySeconds() external view returns (uint256);
        function proofMaturityDelaySeconds() external view returns (uint256);
        function version() external view returns (string);
        function proveWithdrawalTransaction(
            WithdrawalTransaction tx_,
            uint256 disputeGameIndex,
            OutputRootProof outputRootProof,
            bytes[] withdrawalProof
        ) external;
        function finalizeWithdrawalTransaction(WithdrawalTransaction tx_) external;
        function finalizedWithdrawals(bytes32 withdrawalHash) external view returns (bool);
    }

    interface IDisputeGame {}

    #[sol(rpc)]
    interface AnchorStateRegistry {
        function isGameClaimValid(IDisputeGame _game) public view returns (bool);
    }

    #[sol(rpc)]
    interface IDisputeGameFactory {
        event DisputeGameCreated(address indexed disputeProxy, uint32 indexed gameType, bytes32 indexed rootClaim);

        function create(uint32 gameType, bytes32 rootClaim, bytes calldata extraData)
            external
            payable
            returns (address proxy);
        function games(uint32 gameType, bytes32 rootClaim, bytes calldata extraData)
            external
            view
            returns (address proxy, uint64 timestamp);
        function gameAtIndex(uint256 index)
            external
            view
            returns (uint32 gameType, uint64 timestamp, address proxy);
        function gameCount() external view returns (uint256 gameCount);
        function gameImpls(uint32 gameType) external view returns (address impl);
        function initBonds(uint32 gameType) external view returns (uint256 bond);
        function getGameUUID(uint32 gameType, bytes32 rootClaim, bytes calldata extraData)
            external
            pure
            returns (bytes32 uuid);
    }

    #[sol(rpc)]
    interface IMultiProofGame {
        event Challenged(address indexed challenger, uint64 proofDeadline);
        event Proved(uint8 indexed lane, bytes32 indexed rootId, address recipient, uint8 proofBitmap);
        event GameClosed(uint8 bondDistributionMode);
        event Resolved(uint8 indexed status);

        // Reverts reachable from `submitProofLane`. Declared so a failed submission can be
        // classified instead of retried blindly.
        error ClaimAlreadyResolved();
        error InvalidParentGame();
        error GameOver();
        error InvalidLane(uint8 lane);
        error InvalidProof(uint8 lane, bytes32 rootId);
        error DuplicateProofLane(uint8 lane, bytes32 rootId, uint8 proofBitmap);

        // Deployment parameters (immutables on the implementation).
        function PROOF_THRESHOLD() external view returns (uint8);
        function PROOF_LANE_COUNT() external view returns (uint8);
        function domainHash() external view returns (bytes32);
        function rollupConfigHash() external view returns (bytes32);
        function aggregationVKey() external view returns (bytes32);
        function rangeVKeyCommitment() external view returns (bytes32);
        function teeImageId() external view returns (bytes32);
        function blockInterval() external view returns (uint256);
        function challengePeriod() external view returns (uint64);
        function proofPeriod() external view returns (uint64);
        function proposerBond() external view returns (uint256);
        function challengerBond() external view returns (uint256);
        function protocolFeeRecipient() external view returns (address);
        function disputeGameFactory() external view returns (address);
        function anchorStateRegistry() external view returns (address);
        function bondVault() external view returns (address);

        // Proposal context.
        function rootId() external view returns (bytes32);
        function proposalDomainHash() external view returns (bytes32);
        function attempt() external view returns (uint256);
        function parentRef() external view returns (address);
        function startingRootHash() external view returns (bytes32);
        function startingBlockNumber() external view returns (uint256);
        function l2SequenceNumber() external view returns (uint256);
        function l1Head() external view returns (bytes32);
        function l1OriginNumber() external view returns (uint256);
        function rootClaim() external view returns (bytes32);
        function gameCreator() external view returns (address);
        function gameType() external view returns (uint32);
        function extraData() external view returns (bytes memory);
        function wasRespectedGameTypeWhenCreated() external view returns (bool);

        // Game progress. `claimData` follows the `ZKDisputeGame.ProposalStatus` state machine.
        function createdAt() external view returns (uint64);
        function resolvedAt() external view returns (uint64);
        function status() external view returns (uint8);
        function claimData()
            external
            view
            returns (uint8 status, address challenger, uint64 deadline, uint8 proofBitmap, uint8 invalidationReason);
        function invalidationReason() external view returns (uint8);
        function proofBitmap() external view returns (uint8);
        function laneRecipient(uint8 laneId) external view returns (address);
        function challenger() external view returns (address);
        function challengeDeadline() external view returns (uint64);
        function proofDeadline() external view returns (uint64);
        function gameOver() external view returns (bool);
        function resolutionStatus()
            external
            view
            returns (bool resolvable, uint8 outcome, uint8 reason);

        // Mutating entry points.
        function challenge() external returns (uint8 proposalStatus);
        /// `proof` is the compact payload built by `encode_compact_proof`.
        function submitProofLane(bytes calldata proof) external returns (uint8 proposalStatus);
        function resolve() external returns (uint8 status);
        function closeGame() external;

        // Bond settlement.
        function bondDistributionMode() external view returns (uint8);
        function totalBonds() external view returns (uint256);
        function normalModeCredit(address recipient) external view returns (uint256);
        function refundModeCredit(address recipient) external view returns (uint256);
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InitiatedWithdrawal {
    pub transaction: WithdrawalTransaction,
    pub hash: B256,
    pub l2_block: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProveWithdrawal {
    pub transaction: WithdrawalTransaction,
    pub hash: B256,
    pub game_index: u64,
    pub game_l2_block: u64,
    pub game_addr: Address,
    pub proven_at: u64,
}
