// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {IWorldID} from "@world-id-contracts/interfaces/IWorldID.sol";

contract MockWorldIDGroups is IWorldID {
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

    function verifyProof(uint256, uint256, uint256, uint256, uint256[8] memory) external view override {
        if (!verifyProofSuccess) {
            revert("Proof verification failed");
        }
    }
}
