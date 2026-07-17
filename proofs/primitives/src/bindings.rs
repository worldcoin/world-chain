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

        function domainHash() external view returns (bytes32);
        function games(bytes32 proposalKey) external view returns (address);
        function propose(
            address parentRef,
            bytes32 rootClaim,
            uint256 l2BlockNumber
        ) external payable returns (address game, bytes32 rootId);
        function computeProposalKey(
            address parentRef,
            bytes32 rootClaim,
            uint256 l2BlockNumber
        ) external view returns (bytes32);
        function computeRootId(
            address parentRef,
            bytes32 rootClaim,
            uint256 l2BlockNumber,
            bytes32 l1OriginHash,
            uint256 l1OriginNumber
        ) external view returns (bytes32);
    }

    #[sol(rpc)]
    interface IWorldChainProofSystemGame {
        function rootId() external view returns (bytes32);
        function parentRef() external view returns (address);
        function rootClaim() external view returns (bytes32);
        function l2BlockNumber() external view returns (uint256);
        function challengeDeadline() external view returns (uint64);
        function state() external view returns (uint8);
        function invalidationReason() external view returns (uint8);
        function proofBitmap() external view returns (uint8);
        function proofCount() external view returns (uint8);
        function challenge() external payable;
        function submitProofLane(uint8 laneId, bytes calldata proof) external;
        function resolutionStatus() external view returns (bool resolvable, uint8 outcome, uint8 reason);
        function resolve() external returns (uint8 outcome, uint8 reason);
    }

    #[sol(rpc)]
    interface IWorldChainAnchorStateRegistry {
        function currentRootId() external view returns (bytes32);
        function currentRootClaim() external view returns (bytes32);
        function currentL2BlockNumber() external view returns (uint256);
        function anchorGame() external view returns (address);
        function setAnchorState(address game) external;
    }
}
