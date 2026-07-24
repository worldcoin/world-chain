// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

interface IWorldChainProofSystemFactory {
    /// @notice Returns the canonical domain configuration shared by every game created by this factory.
    function domain()
        external
        view
        returns (uint256 chainId, uint256 proofSystemVersion, bytes32 rollupConfigHash, uint256 blockInterval);

    function isFactoryGame(address game) external view returns (bool);
}
