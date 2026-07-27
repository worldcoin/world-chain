// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

interface IWorldChainProofSystemFactory {
    /// @notice Returns the canonical domain configuration shared by every game created by this factory.
    function domain()
        external
        view
        returns (uint256 chainId, uint256 proofSystemVersion, bytes32 rollupConfigHash, uint256 blockInterval);

    function propose(
        address parentRef,
        bytes32 rootClaim,
        uint256 l2BlockNumber,
        bytes32 l1OriginHash,
        uint256 l1OriginNumber,
        uint8 laneId,
        bytes calldata proof
    ) external payable returns (address game, bytes32 rootId);

    function isFactoryGame(address game) external view returns (bool);
}
