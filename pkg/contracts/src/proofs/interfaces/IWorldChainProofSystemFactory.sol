// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

interface IWorldChainProofSystemFactory {
    function isFactoryGame(address game) external view returns (bool);
}
