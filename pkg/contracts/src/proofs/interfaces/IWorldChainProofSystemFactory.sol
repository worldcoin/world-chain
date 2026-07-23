// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Claim, GameId, GameType, Timestamp} from "../DisputeTypes.sol";
import {IDisputeGame} from "./IDisputeGame.sol";

interface IWorldChainProofSystemFactory {
    struct GameSearchResult {
        uint256 index;
        GameId metadata;
        Timestamp timestamp;
        Claim rootClaim;
        bytes extraData;
    }

    /// @notice Returns the canonical domain configuration shared by every game created by this factory.
    function domain()
        external
        view
        returns (uint256 chainId, uint256 proofSystemVersion, bytes32 rollupConfigHash, uint256 blockInterval);

    /// @notice Returns the indexed game selected by OptimismPortal2 when proving a withdrawal.
    function gameAtIndex(uint256 index) external view returns (GameType, Timestamp, IDisputeGame);

    function gameCount() external view returns (uint256);

    /// @notice Finds recent games using the OP Stack ABI consumed by standard withdrawal tooling.
    function findLatestGames(GameType gameType, uint256 start, uint256 n)
        external
        view
        returns (GameSearchResult[] memory);

    /// @notice Resolves game identity from its OP Stack game data for ASR registration checks.
    function games(GameType gameType, Claim rootClaim, bytes calldata extraData)
        external
        view
        returns (IDisputeGame, Timestamp);
}
