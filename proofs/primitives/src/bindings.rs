use alloy_sol_types::sol;

sol! {
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
        function gameCount() external view returns (uint256);
        function gameAtIndex(uint256 index)
            external
            view
            returns (uint32 gameType, uint64 timestamp, address proxy);
        function gameImpls(uint32 gameType) external view returns (address);
        function initBonds(uint32 gameType) external view returns (uint256);
    }

    #[sol(rpc)]
    interface IWorldChainProofSystemGame {
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

        function rootId() external view returns (bytes32);
        function disputeGameFactory() external view returns (address);
        function gameCreator() external view returns (address);
        function challenger() external view returns (address);
        function anchorStateRegistry() external view returns (address);
        function weth() external view returns (address);
        function domain()
            external
            view
            returns (
                uint256 chainId,
                uint256 proofSystemVersion,
                bytes32 rollupConfigHash,
                uint256 blockInterval
            );
        function domainHash() external view returns (bytes32);
        function proposalDomainHash() external view returns (bytes32);
        function wasRespectedGameTypeWhenCreated() external view returns (bool);
        function attempt() external view returns (uint256);
        function parentRef() external view returns (address);
        function startingRootClaim() external view returns (bytes32);
        function startingL2BlockNumber() external view returns (uint256);
        function rootClaim() external view returns (bytes32);
        function l2SequenceNumber() external view returns (uint256);
        function l2BlockNumber() external view returns (uint256);
        function l1Head() external view returns (bytes32);
        function l1OriginNumber() external view returns (uint256);
        function challengeDeadline() external view returns (uint64);
        function challengedAt() external view returns (uint64);
        function proofDeadline() external view returns (uint64);
        function proposerBond() external view returns (uint256);
        function challengerBond() external view returns (uint256);
        function resolvedAt() external view returns (uint64);
        function state() external view returns (uint8);
        function invalidationReason() external view returns (uint8);
        function proofBitmap() external view returns (uint8);
        function proofCount() external view returns (uint8);
        function status() external view returns (uint8);
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

    #[sol(rpc)]
    interface IAnchorStateRegistry {
        function isGameFinalized(address game) external view returns (bool);
        function disputeGameFinalityDelaySeconds() external view returns (uint256);
        function getAnchorRoot()
            external
            view
            returns (bytes32 root, uint256 l2SequenceNumber);
        function blacklistDisputeGame(address game) external;
    }

    #[sol(rpc)]
    interface IDelayedWETH {
        function withdrawals(address owner, address recipient)
            external
            view
            returns (uint256 amount, uint256 timestamp);
    }
}
