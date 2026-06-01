// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "./IWorldChainProofVerifier.sol";

/// @title ITEEProverRegistry
/// @notice Maintains accepted TEE signer identities, the active enclave image
///         measurement, and signer admission, per WIP-1006.
interface ITEEProverRegistry {
    /// @notice Whether `signer` is currently registered for `imageHash`.
    function isRegisteredSigner(address signer, bytes32 imageHash) external view returns (bool);

    /// @notice The currently active enclave image measurement.
    function activeImageHash() external view returns (bytes32);
}

/// @title TEEVerifier
/// @notice WIP-1006 `TEE_ATTESTATION` lane verifier. Accepts an ECDSA signature
///         from a registered TEE signer over the active image hash and `rootId`.
/// @dev The attestation digest is domain-separated so a TEE signature cannot be
///      replayed across lanes or image versions. The active image hash is read
///      from the registry, so rotating the enclave measurement invalidates
///      attestations bound to a stale image.
contract TEEVerifier is IWorldChainProofVerifier {
    /// @notice EIP-191-style domain tag binding signatures to this lane.
    bytes32 public constant TEE_ATTESTATION_DOMAIN = keccak256("WorldChainProofSystem.TEE_ATTESTATION.v1");

    /// @notice The TEE prover registry.
    ITEEProverRegistry public immutable registry;

    /// @notice Thrown when the recovered signer is not registered for the image.
    error UnregisteredSigner(address signer, bytes32 imageHash);

    /// @notice Thrown when the signature is malformed.
    error MalformedSignature();

    /// @param registry_ The TEE prover registry.
    constructor(ITEEProverRegistry registry_) {
        registry = registry_;
    }

    /// @inheritdoc IWorldChainProofVerifier
    /// @dev `proof` is `abi.encode(bytes signature)` over the digest
    ///      `keccak256(TEE_ATTESTATION_DOMAIN, imageHash, rootId)`.
    function verify(bytes32 rootId, bytes calldata proof) external view returns (bool) {
        bytes memory signature = abi.decode(proof, (bytes));
        if (signature.length != 65) revert MalformedSignature();

        bytes32 imageHash = registry.activeImageHash();
        bytes32 digest = keccak256(abi.encode(TEE_ATTESTATION_DOMAIN, imageHash, rootId));

        bytes32 r;
        bytes32 s;
        uint8 v;
        assembly {
            r := mload(add(signature, 0x20))
            s := mload(add(signature, 0x40))
            v := byte(0, mload(add(signature, 0x60)))
        }

        address signer = ecrecover(digest, v, r, s);
        if (signer == address(0)) revert MalformedSignature();
        if (!registry.isRegisteredSigner(signer, imageHash)) revert UnregisteredSigner(signer, imageHash);
        return true;
    }
}
