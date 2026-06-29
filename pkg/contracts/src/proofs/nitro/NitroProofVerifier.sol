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
///      This contract:
///        1. Reconstructs the proposal's `rootId` from the boot-info plus the
///           remaining context fields supplied in the proof and asserts it
///           equals the `rootId` the game is asking about — this binds the
///           Nitro signature to the *specific* proposal under dispute.
///        2. Checks that `expectedPublicKey` is currently registered in
///           {NitroEnclaveKeyRegistry}.
///        3. Recomputes the signing commitment from the boot-info fields.
///        4. Recovers the signer via `ecrecover` and matches it against the
///           Ethereum address derived from `expectedPublicKey`.
///
///      Three entry points are provided:
///        - {verifyProof}             — explicit Nitro-typed API. Caller passes
///                                       boot-info fields directly; the rootId
///                                       binding is the caller's responsibility.
///        - {verifyProofPrecomputed}  — caller already has the 32-byte signing
///                                       commitment.
///        - {verify}                  — the generic {IWorldChainProofVerifier}
///                                       hook used by {WorldChainProofSystemGame}.
///                                       Binds the supplied `rootId` to the
///                                       boot-info fields inside `proof`.
contract NitroProofVerifier is IWorldChainProofVerifier {
    /*//////////////////////////////////////////////////////////////
                                 ERRORS
    //////////////////////////////////////////////////////////////*/

    /// @notice Thrown when {verifyProof} is called with a malformed signature.
    error InvalidSignatureLength();

    /// @notice Thrown when the supplied public key is not a 65-byte
    ///         SEC1-uncompressed secp256k1 key (`0x04 || X || Y`). The World
    ///         Nitro enclave always emits this form.
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
                          SIGNING COMMITMENT
    //////////////////////////////////////////////////////////////*/

    /// @notice Reconstructs the 32-byte commitment the enclave actually signed,
    ///         matching `signing_commitment(boot_info)` in
    ///         `proofs/nitro/src/protocol.rs`.
    /// @dev Layout: `l2PostRoot (32) || uint64BE(l2BlockNumber) (8) ||
    ///      rollupConfigHash (32)`, hashed with keccak256.
    function signingCommitment(bytes32 l2PostRoot, uint64 l2BlockNumber, bytes32 rollupConfigHash)
        public
        pure
        returns (bytes32)
    {
        return keccak256(abi.encodePacked(l2PostRoot, l2BlockNumber, rollupConfigHash));
    }

    /*//////////////////////////////////////////////////////////////
                          PROOF VERIFICATION
    //////////////////////////////////////////////////////////////*/

    /// @notice Verifies a Nitro TEE proof by reconstructing the signing
    ///         commitment from `(l2PostRoot, l2BlockNumber, rollupConfigHash)`
    ///         and recovering the signer.
    /// @dev This entry point does **not** bind the result to a `rootId`. Use
    ///      {verify} (the generic hook) when proposal binding is required.
    function verifyProof(
        bytes32 l2PostRoot,
        uint64 l2BlockNumber,
        bytes32 rollupConfigHash,
        bytes calldata signature,
        bytes calldata expectedPublicKey
    ) external view returns (bool) {
        return verifyProofPrecomputed(
            signingCommitment(l2PostRoot, l2BlockNumber, rollupConfigHash), signature, expectedPublicKey
        );
    }

    /// @notice Lower-level variant of {verifyProof} when the caller has already
    ///         computed the 32-byte signing commitment.
    function verifyProofPrecomputed(bytes32 commitment, bytes calldata signature, bytes calldata expectedPublicKey)
        public
        view
        returns (bool)
    {
        if (expectedPublicKey.length != 65 || expectedPublicKey[0] != 0x04) revert InvalidPublicKey();

        if (!registry.isKeyRegistered(expectedPublicKey)) return false;

        if (signature.length != 65) revert InvalidSignatureLength();

        bytes32 r;
        bytes32 s;
        uint8 v;
        assembly ("memory-safe") {
            r := calldataload(signature.offset)
            s := calldataload(add(signature.offset, 32))
            v := byte(0, calldataload(add(signature.offset, 64)))
        }

        // EIP-2 low-s rejection.
        if (uint256(s) > 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0) {
            return false;
        }
        if (v != 27 && v != 28) return false;

        address recovered = ecrecover(commitment, v, r, s);
        if (recovered == address(0)) return false;

        // Ethereum address = last 20 bytes of keccak256(X || Y), i.e. of the
        // 64-byte tail after the `0x04` prefix.
        bytes32 keyHash = keccak256(expectedPublicKey[1:65]);
        return recovered == address(uint160(uint256(keyHash)));
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
    ///      The verifier rebuilds the {WorldChainProofLib.rootId} from the
    ///      first seven context fields plus `l2PostRoot` (which equals
    ///      `rootClaim`) and `l2BlockNumber`, and asserts it matches the
    ///      `rootId` passed in by the game. This binds the enclave signature
    ///      to the *specific* proposal under dispute.
    ///
    ///      Any decode or verification failure is surfaced as `false` (never
    ///      a revert) to honour the boolean-predicate contract of
    ///      {IWorldChainProofVerifier}.
    function verify(bytes32 rootId, bytes calldata proof) external view returns (bool) {
        try this._decodeAndVerify(rootId, proof) returns (bool ok) {
            return ok;
        } catch {
            return false;
        }
    }

    /// @notice External helper used only by {verify}; MUST NOT be called directly.
    /// @dev External so that {verify} can invoke it via `this.` and trap reverts
    ///      (including the ABI decode revert) in a try/catch.
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

        // 1. Bind the proof to the supplied rootId. The boot_info's l2PostRoot
        //    plays the role of `rootClaim` (the proposal's claimed L2 output
        //    root) in WorldChainProofLib.rootId.
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

        // 2. Verify the enclave signature over the signing commitment derived
        //    from the same boot_info fields.
        return this.verifyProof(l2PostRoot, l2BlockNumber, rollupConfigHash, signature, expectedPublicKey);
    }
}
