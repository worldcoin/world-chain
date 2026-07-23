use alloy_sol_types::sol;

sol! {
    /// Stock Optimism `DisputeGameFactory` surface used by the World Chain proof services.
    ///
    /// `GameType` / `Timestamp` / `Claim` / `Hash` user-defined value types are declared with
    /// their underlying ABI types (uint32 / uint64 / bytes32).
    #[sol(rpc)]
    interface IDisputeGameFactory {
        event DisputeGameCreated(
            address indexed disputeProxy,
            uint32 indexed gameType,
            bytes32 indexed rootClaim
        );

        function create(
            uint32 gameType,
            bytes32 rootClaim,
            bytes calldata extraData
        ) external payable returns (address proxy);
        function games(
            uint32 gameType,
            bytes32 rootClaim,
            bytes calldata extraData
        ) external view returns (address proxy, uint64 timestamp);
        function gameAtIndex(uint256 index)
            external
            view
            returns (uint32 gameType, uint64 timestamp, address proxy);
        function gameCount() external view returns (uint256);
        function gameImpls(uint32 gameType) external view returns (address);
        function initBonds(uint32 gameType) external view returns (uint256);
        function getGameUUID(
            uint32 gameType,
            bytes32 rootClaim,
            bytes calldata extraData
        ) external pure returns (bytes32);
    }

    /// Stock Optimism `AnchorStateRegistry` surface used by the World Chain proof services.
    #[sol(rpc)]
    interface IAnchorStateRegistry {
        function getAnchorRoot() external view returns (bytes32 root, uint256 l2SequenceNumber);
        function anchorGame() external view returns (address);
        function respectedGameType() external view returns (uint32);
        function retirementTimestamp() external view returns (uint64);
        function disputeGameFinalityDelaySeconds() external view returns (uint256);
        function isGameFinalized(address game) external view returns (bool);
        function isGameProper(address game) external view returns (bool);
        function isGameClaimValid(address game) external view returns (bool);
        function isGameBlacklisted(address game) external view returns (bool);
        function setAnchorState(address game) external;
        function paused() external view returns (bool);
    }

    /// `WorldChainProofSystemGame` surface (an `IDisputeGame` implementation).
    #[sol(rpc)]
    interface IWorldChainProofSystemGame {
        /// Rich creation event emitted by the game itself; replaces the deleted factory's
        /// `GameCreated` for offchain indexers.
        event WorldChainGameCreated(
            bytes32 indexed rootId,
            address indexed parentRef,
            bytes32 rootClaim,
            uint256 l2BlockNumber,
            bytes32 l1OriginHash,
            uint256 l1OriginNumber,
            uint256 attempt,
            address proposer
        );
        event Resolved(uint8 indexed status);
        event GameClosed(uint8 bondDistributionMode);

        function rootId() external view returns (bytes32);
        function gameCreator() external view returns (address);
        function gameType() external view returns (uint32);
        function disputeGameFactory() external view returns (address);
        function anchorStateRegistry() external view returns (address);
        function weth() external view returns (address);
        function domainHash() external view returns (bytes32);
        function attempt() external view returns (uint256);
        function parentIndex() external view returns (uint256);
        function parentRef() external view returns (address);
        function startingRootClaim() external view returns (bytes32);
        function startingL2BlockNumber() external view returns (uint256);
        function rootClaim() external view returns (bytes32);
        function l2SequenceNumber() external view returns (uint256);
        function l2BlockNumber() external view returns (uint256);
        function l1OriginHash() external view returns (bytes32);
        function l1OriginNumber() external view returns (uint256);
        function createdAt() external view returns (uint64);
        function resolvedAt() external view returns (uint64);
        function challengeDeadline() external view returns (uint64);
        function proofDeadline() external view returns (uint64);
        function finalizedAt() external view returns (uint64);
        function status() external view returns (uint8);
        function state() external view returns (uint8);
        function invalidationReason() external view returns (uint8);
        function proofBitmap() external view returns (uint8);
        function proofCount() external view returns (uint8);
        function bondDistributionMode() external view returns (uint8);
        function proposerBond() external view returns (uint256);
        function challengerBond() external view returns (uint256);
        function resolutionStatus()
            external
            view
            returns (bool resolvable, uint8 outcome, uint8 reason);
        function resolve() external returns (uint8 status);
        function closeGame() external;
        function credit(address recipient) external view returns (uint256);
        function claimCredit(address recipient) external;
        function challenge() external payable;
        function submitProofLane(uint8 laneId, bytes calldata proof) external;
    }

    /// Stock Optimism `DelayedWETH` surface needed for two-phase credit claims.
    #[sol(rpc)]
    interface IDelayedWETH {
        function delay() external view returns (uint256);
        function withdrawals(address game, address recipient)
            external
            view
            returns (uint256 amount, uint256 timestamp);
        function balanceOf(address account) external view returns (uint256);
    }
}
