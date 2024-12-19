// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {IWorldIDGroups} from "@world-id-contracts/interfaces/IWorldIDGroups.sol";

contract MockWorldIDGroups is IWorldIDGroups {
    bool public verifyProofSuccess = true;

    event VerifyProofCalled(
        uint256 root,
        uint256 groupId,
        uint256 signalHash,
        uint256 nullifierHash,
        uint256 externalNullifierHash,
        uint256[8] proof
    );

    function setVerifyProofSuccess(bool _success) external {
        verifyProofSuccess = _success;
    }

    function verifyProof(
        uint256 root,
        uint256 groupId,
        uint256 signalHash,
        uint256 nullifierHash,
        uint256 externalNullifierHash,
        uint256[8] memory proof
    ) external override {
        emit VerifyProofCalled(root, groupId, signalHash, nullifierHash, externalNullifierHash, proof);
        if (!verifyProofSuccess) {
            revert("Proof verification failed");
        }
    }
}
