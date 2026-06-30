// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {INitroAttestationVerifier} from "../../../src/proofs/nitro/INitroAttestationVerifier.sol";

/// @title MockNitroAttestationVerifier
/// @notice Test double for `INitroAttestationVerifier`. Lets tests preset the
///         `(attestationTbs, signature, hints) -> (publicKey, pcr0, pcr1, pcr2)`
///         mapping so the registry can be exercised without a real Nitro
///         document + cert chain. Mirrors the hinted verifier API.
contract MockNitroAttestationVerifier is INitroAttestationVerifier {
    error UnexpectedCall();

    struct Output {
        bytes publicKey;
        bytes32 pcr0;
        bytes32 pcr1;
        bytes32 pcr2;
    }

    mapping(bytes32 callKey => Output output) private _preset;

    function setExpectation(
        bytes memory attestationTbs,
        bytes memory signature,
        bytes memory publicKey,
        bytes32 pcr0,
        bytes32 pcr1,
        bytes32 pcr2
    ) external {
        // Key on tbs+sig only; hints are not part of the semantic identity.
        _preset[keccak256(abi.encode(attestationTbs, signature))] =
            Output({publicKey: publicKey, pcr0: pcr0, pcr1: pcr1, pcr2: pcr2});
    }

    function verifyAttestation(
        bytes calldata attestationTbs,
        bytes calldata signature,
        bytes calldata /* attestationSigHints */
    ) external view returns (bytes memory publicKey, bytes32 pcr0, bytes32 pcr1, bytes32 pcr2) {
        Output memory out = _preset[keccak256(abi.encode(attestationTbs, signature))];
        if (out.publicKey.length == 0) revert UnexpectedCall();
        return (out.publicKey, out.pcr0, out.pcr1, out.pcr2);
    }
}
