use alloy_sol_types::sol;

sol! {
    #[sol(rpc)]
    interface IWorldChainProofSystemFactory {
        event GameCreated(
            bytes32 indexed proposalKey,
            bytes32 indexed rootId,
            address indexed game,
            address proposer,
            bytes32 rootClaim,
            uint256 l2BlockNumber,
            address parentRef,
            bytes32 l1OriginHash,
            uint256 l1OriginNumber
        );

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
        function proposerBond() external view returns (uint256);
        function challengerBond() external view returns (uint256);
        function games(
            uint32 gameType,
            bytes32 rootClaim,
            bytes calldata extraData
        ) external view returns (address game, uint64 timestamp);
        function gameCount() external view returns (uint256);
        function gameAtIndex(uint256 index)
            external
            view
            returns (uint32 gameType, uint64 timestamp, address game);
        function propose(
            address parentRef,
            bytes32 rootClaim,
            uint256 l2BlockNumber
        ) external payable returns (address game, bytes32 rootId);
    }

    #[sol(rpc)]
    interface IWorldChainProofSystemGame {
        event Withdrawn(address indexed recipient, uint256 amount);

        function rootId() external view returns (bytes32);
        function factory() external view returns (address);
        function gameCreator() external view returns (address);
        function challenger() external view returns (address);
        function anchorStateRegistry() external view returns (address);
        function domainHash() external view returns (bytes32);
        function attempt() external view returns (uint256);
        function parentRef() external view returns (address);
        function startingRootClaim() external view returns (bytes32);
        function startingL2BlockNumber() external view returns (uint256);
        function rootClaim() external view returns (bytes32);
        function l2SequenceNumber() external view returns (uint256);
        function l1Head() external view returns (bytes32);
        function l1OriginNumber() external view returns (uint256);
        function challengeDeadline() external view returns (uint64);
        function proofDeadline() external view returns (uint64);
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
        function resolve() external returns (uint8 outcome, uint8 reason);
        function closeGame() external;
        function claimable(address recipient) external view returns (uint256);
        function withdraw(address payable recipient) external;
        function challenge() external payable;
        function submitProofLane(uint8 laneId, bytes calldata proof) external;
    }

    #[sol(rpc)]
    interface IWorldChainAnchorStateRegistry {
        function setAnchorState(address game) external;
        function isGameFinalized(address game) external view returns (bool);
        function isGameClaimValid(address game) external view returns (bool);
        function disputeGameFactory() external view returns (address);
        function paused() external view returns (bool);
        function getAnchorRoot()
            external
            view
            returns (bytes32 root, uint256 l2SequenceNumber);
        function anchorGame() external view returns (address);
        function isGameBlacklisted(address game) external view returns (bool);
        function setGameBlacklisted(address game, bool blacklisted) external;
    }
}
