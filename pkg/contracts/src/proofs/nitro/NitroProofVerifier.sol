// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "../interfaces/IWorldChainProofVerifier.sol";
import {NitroEnclaveKeyRegistry} from "./NitroEnclaveKeyRegistry.sol";

/// @title NitroProofVerifier
/// @author Worldcoin
/// @notice TEE-attestation proof lane verifier compatible with WIP-1006's multi-proof
///         system ({IWorldChainProofVerifier}).
/// @dev The enclave produces an ECDSA (secp256k1) signature over a `signingCommitment`
///      using its NSM-certified key. This contract checks that:
///        1. the supplied `expectedPublicKey` is currently registered in
///           {NitroEnclaveKeyRegistry}; and
///        2. `ecrecover(signingCommitment, signature)` matches the Ethereum address
///           derived from `expectedPublicKey`.
///
///      Two entry points are provided:
///        - {verifyProof}      — the explicit Nitro-typed API.
///        - {verify}           — the generic {IWorldChainProofVerifier} hook used by
///                                {WorldChainProofSystemGame}. The `proof` calldata
///                                is `abi.encode(signature, expectedPublicKey)`.
contract NitroProofVerifier is IWorldChainProofVerifier {
    /*//////////////////////////////////////////////////////////////
                                 ERRORS
    //////////////////////////////////////////////////////////////*/

    /// @notice Thrown when {verifyProof} is called with a malformed signature.
    error InvalidSignatureLength();

    /// @notice Thrown when the supplied public key is not a 65-byte SEC1-uncompressed
    ///         secp256k1 key (`0x04 || X || Y`).
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
                          PROOF VERIFICATION
    //////////////////////////////////////////////////////////////*/

    /// @notice Verifies that `signature` was produced by the enclave key
    ///         `expectedPublicKey` over `signingCommitment`, and that this key is
    ///         currently registered with {NitroEnclaveKeyRegistry}.
    ///
    /// @param signingCommitment The 32-byte commitment that was signed (typically the
    ///                          `rootId` produced by `WorldChainProofLib`).
    /// @param signature         A 65-byte ECDSA signature (`r || s || v`).
    /// @param expectedPublicKey The 65-byte SEC1-uncompressed secp256k1 enclave key.
    ///
    /// @return ok `true` if the signature is valid and the key is registered.
    function verifyProof(
        bytes32 signingCommitment,
        bytes calldata signature,
        bytes calldata expectedPublicKey
    ) external view returns (bool ok) {
        if (!registry.isKeyRegistered(expectedPublicKey)) return false;
        if (expectedPublicKey.length != 65 || expectedPublicKey[0] != 0x04) {
            revert InvalidPublicKey();
        }
        if (signature.length != 65) revert InvalidSignatureLength();

        bytes32 r;
        bytes32 s;
        uint8 v;
        // Calldata layout: signature[0..32] = r, [32..64] = s, [64] = v.
        assembly ("memory-safe") {
            r := calldataload(signature.offset)
            s := calldataload(add(signature.offset, 32))
            v := byte(0, calldataload(add(signature.offset, 64)))
        }

        // Reject signatures with high-s as per EIP-2 to prevent malleability.
        if (uint256(s) > 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0) {
            return false;
        }
        if (v != 27 && v != 28) return false;

        address recovered = ecrecover(signingCommitment, v, r, s);
        if (recovered == address(0)) return false;

        // Ethereum address = last 20 bytes of keccak256(pubkey[1:65]).
        // The 0x04 prefix is excluded.
        address expected = address(uint160(uint256(keccak256(expectedPublicKey[1:65]))));
        return recovered == expected;
    }

    /// @inheritdoc IWorldChainProofVerifier
    /// @dev `proof` is the ABI encoding `abi.encode(bytes signature, bytes
    ///      expectedPublicKey)` produced by the off-chain prover. Reverts that occur
    ///      inside {verifyProof} are caught and surfaced as `false` so that this
    ///      hook obeys the verifier contract's "boolean predicate" semantics.
    function verify(bytes32 rootId, bytes calldata proof) external view returns (bool) {
        (bytes memory signature, bytes memory expectedPublicKey) = abi.decode(proof, (bytes, bytes));
        try this.verifyProof(rootId, signature, expectedPublicKey) returns (bool ok) {
            return ok;
        } catch {
            return false;
        }
    }
}
