// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IWorldID} from "@world-id-contracts/interfaces/IWorldID.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";
import {IMulticall3} from "./IMulticall3.sol";

interface IPBHEntryPoint {
    /// @notice The Packed World ID Proof data.
    /// @param root The root of the Merkle tree.
    /// @param pbhExternalNullifier The external nullifier for the PBH User Operation.
    /// @param nullifierHash The nullifier hash for the PBH User Operation.
    /// @param proof The Semaphore proof.
    struct PBHPayload {
        uint256 root;
        uint256 pbhExternalNullifier;
        uint256 nullifierHash;
        uint256[8] proof;
    }

    function handleAggregatedOps(
        IEntryPoint.UserOpsPerAggregator[] calldata opsPerAggregator,
        address payable beneficiary
    ) external;

    function pbhMulticall(IMulticall3.Call3[] calldata calls, PBHPayload calldata pbhPayload)
        external
        returns (IMulticall3.Result[] memory returnData);

    function initialize(
        IWorldID worldId,
        IEntryPoint entryPoint,
        uint8 _numPbhPerMonth,
        address _multicall3,
        uint256 _multicallGasLimit
    ) external;

    function validateSignaturesCallback(bytes32 hashedOps) external view;

    function verifyPbh(uint256 signalHash, PBHPayload calldata pbhPayload) external view;

    function nullifierHashes(uint256) external view returns (bool);

    function setNumPbhPerMonth(uint8 _numPbhPerMonth) external;

    function setWorldId(address _worldId) external;
}
