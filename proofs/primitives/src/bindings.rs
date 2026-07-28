use alloy_sol_types::sol;

sol! {
    /// Stock OP Stack `DisputeGameFactory`. WIP-1006 games are created and indexed here
    /// alongside every other game type registered on the chain, so every index-based read
    /// must filter on `MULTI_PROOF_GAME_TYPE`.
    #[sol(rpc)]
    interface IDisputeGameFactory {
        struct GameSearchResult {
            uint256 index;
            bytes32 metadata;
            uint64 timestamp;
            bytes32 rootClaim;
            bytes extraData;
        }

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
        function findLatestGames(uint32 gameType, uint256 start, uint256 count)
            external
            view
            returns (GameSearchResult[] memory games);
        function gameImpls(uint32 gameType) external view returns (address impl);
        function initBonds(uint32 gameType) external view returns (uint256 bond);
        function getGameUUID(uint32 gameType, bytes32 rootClaim, bytes calldata extraData)
            external
            pure
            returns (bytes32 uuid);
    }

    /// WIP-1006 `MultiProofGame`: the stock `IDisputeGame` surface plus the World Chain
    /// proof-lane extensions.
    #[sol(rpc)]
    interface IMultiProofGame {
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
        event Challenged(address indexed challenger, uint64 proofDeadline);
        event ProofLaneSupported(uint8 indexed lane, bytes32 indexed rootId, uint8 proofBitmap);
        event ProofThresholdReached(bytes32 indexed rootId, uint8 proofBitmap);
        event DuplicateProofLane(uint8 indexed lane, bytes32 indexed rootId, uint8 proofBitmap);
        event GameClosed(uint8 bondDistributionMode);
        event Resolved(uint8 indexed status);

        // Deployment parameters (immutables on the implementation).
        function PROOF_THRESHOLD() external view returns (uint8);
        function PROOF_LANE_COUNT() external view returns (uint8);
        function domainHash() external view returns (bytes32);
        function challengePeriod() external view returns (uint64);
        function proofPeriod() external view returns (uint64);
        function proposerBond() external view returns (uint256);
        function challengerBond() external view returns (uint256);
        function disputeGameFactory() external view returns (address);
        function anchorStateRegistry() external view returns (address);
        function weth() external view returns (address);

        // Proposal context.
        function rootId() external view returns (bytes32);
        function proposalDomainHash() external view returns (bytes32);
        function attempt() external view returns (uint256);
        function retryOf() external view returns (address);
        function creationProof() external view returns (bytes memory);
        function parentRef() external view returns (address);
        function startingRootClaim() external view returns (bytes32);
        function startingL2BlockNumber() external view returns (uint256);
        function l2BlockNumber() external view returns (uint256);
        function l2SequenceNumber() external view returns (uint256);
        function l1Head() external view returns (bytes32);
        function l1OriginHash() external view returns (bytes32);
        function l1OriginNumber() external view returns (uint256);
        function rootClaim() external view returns (bytes32);
        function gameCreator() external view returns (address);
        function gameType() external view returns (uint32);
        function extraData() external view returns (bytes memory);
        function wasRespectedGameTypeWhenCreated() external view returns (bool);

        // Game progress.
        function createdAt() external view returns (uint64);
        function resolvedAt() external view returns (uint64);
        function status() external view returns (uint8);
        function state() external view returns (uint8);
        function invalidationReason() external view returns (uint8);
        function proofBitmap() external view returns (uint8);
        function proofCount() external view returns (uint8);
        function challenger() external view returns (address);
        function challengeDeadline() external view returns (uint64);
        function proofDeadline() external view returns (uint64);
        function challengedAt() external view returns (uint64);
        function finalizedAt() external view returns (uint64);
        function invalidatedAt() external view returns (uint64);
        function resolutionStatus()
            external
            view
            returns (bool resolvable, uint8 outcome, uint8 reason);

        // Mutating entry points.
        function challenge() external payable;
        function submitProofLane(uint8 laneId, bytes calldata proof) external;
        function resolve() external returns (uint8 status);
        function closeGame() external;
        function claimCredit(address recipient) external;

        // Bond settlement.
        function bondDistributionMode() external view returns (uint8);
        function totalBonds() external view returns (uint256);
        function normalModeCredit(address recipient) external view returns (uint256);
        function refundModeCredit(address recipient) external view returns (uint256);
        function credit(address recipient) external view returns (uint256);
    }

    /// Stock OP Stack `AnchorStateRegistry`.
    #[sol(rpc)]
    interface IAnchorStateRegistry {
        function getAnchorRoot() external view returns (bytes32 root, uint256 l2SequenceNumber);
        function anchorGame() external view returns (address);
        function disputeGameFactory() external view returns (address);
        function respectedGameType() external view returns (uint32);
        function isGameBlacklisted(address game) external view returns (bool);
        function isGameRetired(address game) external view returns (bool);
        function isGameProper(address game) external view returns (bool);
        function isGameResolved(address game) external view returns (bool);
        function isGameFinalized(address game) external view returns (bool);
        function isGameClaimValid(address game) external view returns (bool);
        function setAnchorState(address game) external;
        function paused() external view returns (bool);
    }

    /// Stock OP Stack `DelayedWETH`, the bond custody contract behind every game.
    #[sol(rpc)]
    interface IDelayedWETH {
        function delay() external view returns (uint256);
        function withdrawals(address owner, address recipient)
            external
            view
            returns (uint256 amount, uint256 timestamp);
    }
}
