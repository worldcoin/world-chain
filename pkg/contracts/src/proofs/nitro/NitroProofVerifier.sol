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
///        3. Checks that `expectedPublicKey` is currently registered in
///           `NitroEnclaveKeyRegistry`.
///        4. Recomputes the signing commitment from all transition public values.
///        5. Recovers the signer via `ecrecover` and matches it against the
///           Ethereum address derived from `expectedPublicKey`.
///
///      Any decode or verification failure is surfaced as `false` (never
///      a revert) to honour the boolean-predicate contract of
///      `IWorldChainProofVerifier`.
contract NitroProofVerifier is IWorldChainProofVerifier {
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
    ///            bytes   signature,
    ///            bytes   expectedPublicKey
    ///        )
    ///
    ///      Decoding and binding live behind an external `this.` call, so a
    ///      revert there is unambiguously a malformed payload. The key-registry
    ///      lookup is then made separately, so a registry outage reports
    ///      `UNAVAILABLE` instead of masquerading as an unregistered signer.
    function verify(bytes32 rootId, bytes calldata proof) external view returns (ProofLib.VerificationStatus) {
        ProofLib.VerificationStatus binding;
        ProofLib.TransitionPublicValues memory transition;
        bytes memory signature;
        bytes memory expectedPublicKey;
        try this._decodeAndBind(msg.sender, rootId, proof) returns (
            ProofLib.VerificationStatus binding_,
            ProofLib.TransitionPublicValues memory transition_,
            bytes memory signature_,
            bytes memory expectedPublicKey_
        ) {
            binding = binding_;
            transition = transition_;
            signature = signature_;
            expectedPublicKey = expectedPublicKey_;
        } catch {
            return ProofLib.VerificationStatus.MALFORMED;
        }
        if (binding != ProofLib.VerificationStatus.VALID) return binding;

        // A structurally wrong key is the submitter's error; the registry never sees it.
        if (expectedPublicKey.length != 65 || expectedPublicKey[0] != 0x04) {
            return ProofLib.VerificationStatus.MALFORMED;
        }
        if (signature.length != 65) return ProofLib.VerificationStatus.MALFORMED;

        // The registry is a live dependency. If it cannot answer, the signature is unjudged.
        if (address(registry).code.length == 0) return ProofLib.VerificationStatus.UNAVAILABLE;
        bool registered;
        try registry.isKeyRegistered(expectedPublicKey) returns (bool registered_) {
            registered = registered_;
        } catch {
            return ProofLib.VerificationStatus.UNAVAILABLE;
        }
        if (!registered) return ProofLib.VerificationStatus.REJECTED;

        bytes32 commitment = _signingCommitment(transition);
        return _verifyEnclaveSignature(commitment, signature, expectedPublicKey)
            ? ProofLib.VerificationStatus.VALID
            : ProofLib.VerificationStatus.REJECTED;
    }

    /// @notice External helper used only by `verify`; MUST NOT be called
    ///         directly.
    /// @dev External so that `verify` can invoke it via `this.` and trap the
    ///      ABI decode revert. Performs no cryptography and touches no
    ///      external dependency — it only decodes and binds to the game.
    function _decodeAndBind(address gameAddress, bytes32 rootId, bytes calldata proof)
        external
        view
        returns (
            ProofLib.VerificationStatus status,
            ProofLib.TransitionPublicValues memory transition,
            bytes memory signature,
            bytes memory expectedPublicKey
        )
    {
        require(msg.sender == address(this), "internal");
        bytes32 domainHash;
        address parentRef;
        uint256 l1OriginNumber;
        (domainHash, parentRef, l1OriginNumber, transition, signature, expectedPublicKey) =
            abi.decode(proof, (bytes32, address, uint256, ProofLib.TransitionPublicValues, bytes, bytes));

        status =
            ProofVerificationLib.matchesGame(gameAddress, rootId, domainHash, parentRef, l1OriginNumber, transition);
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

    /// @dev Checks that `signature` over `commitment` recovers to the address
    ///      derived from `expectedPublicKey`. Purely cryptographic: `verify`
    ///      has already established that the key and signature are structurally
    ///      well-formed and that the key is registered, so every `false` here
    ///      is a genuine signature rejection.
    function _verifyEnclaveSignature(bytes32 commitment, bytes memory signature, bytes memory expectedPublicKey)
        internal
        pure
        returns (bool)
    {
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
        // ecrecover returns address(0) for malformed (r, s, v) tuples that
        // it cannot decode. Reject explicitly: matching against an
        // expectedPublicKey that derives to address(0) is cryptographically
        // impossible, but a defensive check here keeps the failure mode
        // explicit and audit-friendly.
        if (recovered == address(0)) return false;

        // Ethereum address = last 20 bytes of keccak256(X || Y), i.e. of the
        // 64-byte tail after the `0x04` prefix.
        bytes32 keyHash;
        assembly ("memory-safe") {
            // expectedPublicKey[1:65] is 64 bytes starting at +33 (length word + prefix).
            keyHash := keccak256(add(expectedPublicKey, 33), 64)
        }
        return recovered == address(uint160(uint256(keyHash)));
    }
}
