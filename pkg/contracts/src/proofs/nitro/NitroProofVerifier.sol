// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "../interfaces/IWorldChainProofVerifier.sol";
import {ProofLib} from "../lib/ProofLib.sol";
import {ProofVerificationLib} from "../lib/ProofVerificationLib.sol";
import {NitroEnclaveKeyRegistry} from "./NitroEnclaveKeyRegistry.sol";

/// @title NitroProofVerifier
/// @author Worldcoin
/// @notice TEE-attestation proof lane verifier compatible with WIP-1006's
///         multi-proof system (`IWorldChainProofVerifier`).
/// @dev The enclave produces an ECDSA (secp256k1) signature over the
///      `transition_commitment` computed in `proofs/nitro/src/protocol.rs`:
///
///         signingCommitment = keccak256(abi.encode(transitionPublicValues))
///
///      The `verify` hook — the only public entry point on this contract —:
///        1. Reconstructs the proposal's `rootId` from the transition public values plus the
///           remaining context fields supplied in the proof and asserts it
///           equals the `rootId` the game is asking about. This binds the
///           Nitro signature to the *specific* proposal under dispute.
///        2. Binds the proposal transition fields to the calling game's immutable snapshot.
///        3. Recomputes the signing commitment from all transition public values.
///        4. Recovers the signer via `ecrecover` and checks that address against
///           `NitroEnclaveKeyRegistry`.
///
///      Any decode or verification failure is surfaced as `false` (never
///      a revert) to honour the boolean-predicate contract of
///      `IWorldChainProofVerifier`.
contract NitroProofVerifier is IWorldChainProofVerifier {
    /*//////////////////////////////////////////////////////////////
                                 ERRORS
    //////////////////////////////////////////////////////////////*/

    /// @dev Thrown when the proof's signature bytes are not exactly 65 bytes.
    ///      Surfaced as `false` via `verify`'s try/catch.
    error InvalidSignatureLength();

    /*//////////////////////////////////////////////////////////////
                                STORAGE
    //////////////////////////////////////////////////////////////*/

    /// @notice Registry of attested enclave keys.
    NitroEnclaveKeyRegistry public immutable registry;

    /*//////////////////////////////////////////////////////////////
                              CONSTRUCTOR
    //////////////////////////////////////////////////////////////*/

    /// @param registry_ The Nitro enclave key registry to consult.
    constructor(NitroEnclaveKeyRegistry registry_) {
        registry = registry_;
    }

    /*//////////////////////////////////////////////////////////////
                         GENERIC VERIFIER HOOK
    //////////////////////////////////////////////////////////////*/

    /// @inheritdoc IWorldChainProofVerifier
    /// @dev `proof` layout (ABI-encoded):
    ///
    ///        (
    ///            bytes32 domainHash,
    ///            address parentRef,
    ///            uint256 l1OriginNumber,
    ///            TransitionPublicValues transitionPublicValues,
    ///            bytes   signature
    ///        )
    ///
    ///      Decoding plus `_verifyDecoded` live behind an external `this.`
    ///      call so the try/catch in `verify` traps every revert path —
    ///      including a malformed ABI payload — and surfaces it as `false`.
    function verify(bytes32 rootId, bytes calldata proof) external view returns (bool) {
        try this._decodeAndVerify(msg.sender, rootId, proof) returns (bool ok) {
            return ok;
        } catch {
            return false;
        }
    }

    /// @notice External helper used only by `verify`; MUST NOT be called
    ///         directly.
    /// @dev External so that `verify` can invoke it via `this.` and trap
    ///      reverts (including the ABI decode revert) in a try/catch.
    function _decodeAndVerify(address gameAddress, bytes32 rootId, bytes calldata proof) external view returns (bool) {
        require(msg.sender == address(this), "internal");
        (
            bytes32 domainHash,
            address parentRef,
            uint256 l1OriginNumber,
            ProofLib.TransitionPublicValues memory transition,
            bytes memory signature
        ) = abi.decode(proof, (bytes32, address, uint256, ProofLib.TransitionPublicValues, bytes));

        // 1. Bind the proof identity and transition fields to the calling game's immutable snapshot.
        bool matchesGame =
            ProofVerificationLib.matchesGame(gameAddress, rootId, domainHash, parentRef, l1OriginNumber, transition);
        if (!matchesGame) return false;

        // 2. Verify the enclave signature over all transition public values.
        bytes32 commitment = _signingCommitment(transition);
        return _verifyEnclaveSignature(commitment, signature);
    }

    /*//////////////////////////////////////////////////////////////
                                INTERNAL
    //////////////////////////////////////////////////////////////*/

    /// @dev Reconstructs the 32-byte commitment the enclave actually signed,
    ///      matching `transition_commitment(transition_public_values)` in
    ///      `proofs/nitro/src/protocol.rs`.
    ///      The entire struct is ABI-encoded before hashing.
    function _signingCommitment(ProofLib.TransitionPublicValues memory transition) internal pure returns (bytes32) {
        return keccak256(abi.encode(transition));
    }

    /// @dev Checks that `signature` over `commitment` recovers to a registered signer.
    ///      Returns `false` on logical mismatch (unregistered signer, malleable s,
    ///      etc.); reverts on a structurally invalid signature length, which
    ///      `verify` catches and turns into `false`.
    function _verifyEnclaveSignature(bytes32 commitment, bytes memory signature) internal view returns (bool) {
        if (signature.length != 65) revert InvalidSignatureLength();

        bytes32 r;
        bytes32 s;
        uint8 v;
        assembly ("memory-safe") {
            // `signature` is `bytes memory`; the data starts at signature+32.
            let p := add(signature, 32)
            r := mload(p)
            s := mload(add(p, 32))
            v := byte(0, mload(add(p, 64)))
        }

        // EIP-2 low-s rejection.
        if (uint256(s) > 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0) {
            return false;
        }
        if (v != 27 && v != 28) return false;

        address recovered = ecrecover(commitment, v, r, s);
        // ecrecover returns address(0) for malformed (r, s, v) tuples.
        if (recovered == address(0)) return false;
        return registry.isSignerRegistered(recovered);
    }
}
