// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {INitroAttestationVerifier} from "../../../src/proofs/nitro/INitroAttestationVerifier.sol";

/// @title MockNitroAttestationVerifier
/// @notice Test double for {INitroAttestationVerifier}. Lets tests preset the
///         (attestationTbs, pcrs) -> publicKey mapping so the registry can be
///         exercised without a real Nitro document + cert chain.
contract MockNitroAttestationVerifier is INitroAttestationVerifier {
    error UnexpectedCall();

    mapping(bytes32 expectedHash => bytes publicKey) public preset;

    function setExpectation(
        bytes memory attestationTbs,
        bytes memory signature,
        bytes32 pcr0,
        bytes32 pcr1,
        bytes32 pcr2,
        bytes memory publicKey
    ) external {
        preset[keccak256(abi.encode(attestationTbs, signature, pcr0, pcr1, pcr2))] = publicKey;
    }

    function verifyAttestation(
        bytes calldata attestationTbs,
        bytes calldata signature,
        bytes32 pcr0,
        bytes32 pcr1,
        bytes32 pcr2
    ) external view returns (bytes memory publicKey) {
        publicKey = preset[keccak256(abi.encode(attestationTbs, signature, pcr0, pcr1, pcr2))];
        if (publicKey.length == 0) revert UnexpectedCall();
    }
}
