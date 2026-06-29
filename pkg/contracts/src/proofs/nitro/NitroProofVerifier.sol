// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "../interfaces/IWorldChainProofVerifier.sol";
import {WorldChainProofLib} from "../WorldChainProofLib.sol";
import {NitroEnclaveKeyRegistry} from "./NitroEnclaveKeyRegistry.sol";

/// @title NitroProofVerifier
/// @author Worldcoin
/// @notice TEE-attestation proof lane verifier compatible with WIP-1006's
///         multi-proof system ({IWorldChainProofVerifier}).
/// @dev The enclave produces an ECDSA (secp256k1) signature over the
///      `signing_commitment` computed in `proofs/nitro/src/protocol.rs`:
///
///         signingCommitment =
///             keccak256( l2PostRoot || uint64BE(l2BlockNumber) || rollupConfigHash )
///
///      The {verify} hook \u2014 the only public entry point on this contract \u2014:
///        1. Reconstructs the proposal's `rootId` from the boot-info plus the
///           remaining context fields supplied in the proof and asserts it
///           equals the `rootId` the game is asking about. This binds the
///           Nitro signature to the *specific* proposal under dispute.
///        2. Checks that `expectedPublicKey` is currently registered in
///           {NitroEnclaveKeyRegistry}.
///        3. Recomputes the signing commitment from the boot-info fields.
///        4. Recovers the signer via `ecrecover` and matches it against the
///           Ethereum address derived from `expectedPublicKey`.
///
///      Any decode or verification failure is surfaced as `false` (never
///      a revert) to honour the boolean-predicate contract of
///      {IWorldChainProofVerifier}.
contract NitroProofVerifier is IWorldChainProofVerifier {
    /*//////////////////////////////////////////////////////////////
                                 ERRORS
    //////////////////////////////////////////////////////////////*/

    /// @dev Thrown when the proof's signature bytes are not exactly 65 bytes.
    ///      Surfaced as `false` via {verify}'s try/catch.
    error InvalidSignatureLength();

    /// @dev Thrown when the proof's expected public key is not a 65-byte
    ///      SEC1-uncompressed secp256k1 key (`0x04 || X || Y`). The World
    ///      Nitro enclave always emits this form.
    error InvalidPublicKey();

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
    ///            bytes32 intermediateRootsHash,
    ///            bytes32 l1OriginHash,
    ///            uint256 l1OriginNumber,
    ///            bytes32 rollupConfigHash,
    ///            bytes32 l2PostRoot,
    ///            uint64  l2BlockNumber,
    ///            bytes   signature,
    ///            bytes   expectedPublicKey
    ///        )
    ///
    ///      Decoding plus {_verifyDecoded} live behind an external `this.`
    ///      call so the try/catch in {verify} traps every revert path \u2014
    ///      including a malformed ABI payload \u2014 and surfaces it as `false`.
    function verify(bytes32 rootId, bytes calldata proof) external view returns (bool) {
        try this._decodeAndVerify(rootId, proof) returns (bool ok) {
            return ok;
        } catch {
            return false;
        }
    }

    /// @notice External helper used only by {verify}; MUST NOT be called
    ///         directly.
    /// @dev External so that {verify} can invoke it via `this.` and trap
    ///      reverts (including the ABI decode revert) in a try/catch.
    function _decodeAndVerify(bytes32 rootId, bytes calldata proof) external view returns (bool) {
        require(msg.sender == address(this), "internal");
        (
            bytes32 domainHash,
            address parentRef,
            bytes32 intermediateRootsHash,
            bytes32 l1OriginHash,
            uint256 l1OriginNumber,
            bytes32 rollupConfigHash,
            bytes32 l2PostRoot,
            uint64 l2BlockNumber,
            bytes memory signature,
            bytes memory expectedPublicKey
        ) = abi.decode(proof, (bytes32, address, bytes32, bytes32, uint256, bytes32, bytes32, uint64, bytes, bytes));

        // 1. Bind the proof to the supplied rootId. The boot_info's
        //    `l2PostRoot` plays the role of `rootClaim` (the proposal's
        //    claimed L2 output root) in WorldChainProofLib.rootId.
        bytes32 expectedRootId = WorldChainProofLib.rootId(
            domainHash,
            parentRef,
            l2PostRoot,
            uint256(l2BlockNumber),
            intermediateRootsHash,
            l1OriginHash,
            l1OriginNumber
        );
        if (expectedRootId != rootId) return false;

        // 2. Verify the enclave signature over the signing commitment
        //    derived from the same boot_info fields.
        bytes32 commitment = _signingCommitment(l2PostRoot, l2BlockNumber, rollupConfigHash);
        return _verifyEnclaveSignature(commitment, signature, expectedPublicKey);
    }

    /*//////////////////////////////////////////////////////////////
                                INTERNAL
    //////////////////////////////////////////////////////////////*/

    /// @dev Reconstructs the 32-byte commitment the enclave actually signed,
    ///      matching `signing_commitment(boot_info)` in
    ///      `proofs/nitro/src/protocol.rs`.
    ///      Layout: `l2PostRoot (32) || uint64BE(l2BlockNumber) (8) ||
    ///      rollupConfigHash (32)`, hashed with keccak256.
    function _signingCommitment(bytes32 l2PostRoot, uint64 l2BlockNumber, bytes32 rollupConfigHash)
        internal
        pure
        returns (bytes32)
    {
        return keccak256(abi.encodePacked(l2PostRoot, l2BlockNumber, rollupConfigHash));
    }

    /// @dev Checks that `signature` over `commitment` recovers to the address
    ///      derived from `expectedPublicKey`, and that the key is registered.
    ///      Returns `false` on logical mismatch (unregistered key, wrong
    ///      signer, malleable s, etc.); reverts on structural errors
    ///      (`InvalidSignatureLength`, `InvalidPublicKey`) which {verify}
    ///      catches and turns into `false`.
    function _verifyEnclaveSignature(bytes32 commitment, bytes memory signature, bytes memory expectedPublicKey)
        internal
        view
        returns (bool)
    {
        if (expectedPublicKey.length != 65 || expectedPublicKey[0] != 0x04) revert InvalidPublicKey();

        if (!_isKeyRegistered(expectedPublicKey)) return false;

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

    /// @dev Memory-bytes wrapper around the calldata-only `isKeyRegistered`
    ///      view on the registry.
    function _isKeyRegistered(bytes memory publicKey) internal view returns (bool) {
        return registry.isKeyRegistered(publicKey);
    }
}
