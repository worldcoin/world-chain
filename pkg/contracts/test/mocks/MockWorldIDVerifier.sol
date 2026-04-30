// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IWorldIDVerifier} from "../../src/interfaces/IWorldIDVerifier.sol";

/// @title MockWorldIDVerifier
/// @notice Test double for `IWorldIDVerifier`. Pure accept/reject toggle — recording call args
///         from inside `verify` / `verifySession` is impossible because both are `view`, so the
///         calling contract dispatches via `STATICCALL`. Tests assert what the manager passed by
///         using `vm.expectCall(verifier, abi.encodeCall(IWorldIDVerifier.verify, (...)))`.
contract MockWorldIDVerifier is IWorldIDVerifier {
    error MockVerifierRejected();

    bool public shouldAccept;

    constructor(bool shouldAccept_) {
        shouldAccept = shouldAccept_;
    }

    function setShouldAccept(bool shouldAccept_) external {
        shouldAccept = shouldAccept_;
    }

    function verify(
        uint256, // nullifier
        uint256, // action
        uint64, // rpId
        uint256, // nonce
        uint256, // signalHash
        uint64, // expiresAtMin
        uint64, // issuerSchemaId
        uint256, // credentialGenesisIssuedAtMin
        uint256[5] calldata // zeroKnowledgeProof
    )
        external
        view
    {
        if (!shouldAccept) revert MockVerifierRejected();
    }

    function verifySession(
        uint64, // rpId
        uint256, // nonce
        uint256, // signalHash
        uint64, // expiresAtMin
        uint64, // issuerSchemaId
        uint256, // credentialGenesisIssuedAtMin
        uint256, // sessionId
        uint256[2] calldata, // sessionNullifier
        uint256[5] calldata // zeroKnowledgeProof
    ) external view {
        if (!shouldAccept) revert MockVerifierRejected();
    }
}
