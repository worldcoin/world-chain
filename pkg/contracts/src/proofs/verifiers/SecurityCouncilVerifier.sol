// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "./IWorldChainProofVerifier.sol";

/// @title SecurityCouncilVerifier
/// @notice WIP-1006 `SECURITY_COUNCIL` lane verifier. Verifies a threshold of
///         distinct council member signatures over `rootId`.
/// @dev Council signatures are domain-separated from other governance actions.
///      A council attestation counts as exactly one lane and cannot finalize a
///      challenged root by itself (enforced by `PROOF_THRESHOLD` in the game).
contract SecurityCouncilVerifier is IWorldChainProofVerifier {
    /// @notice Domain tag binding signatures to the council attestation action.
    bytes32 public constant SECURITY_COUNCIL_DOMAIN = keccak256("WorldChainProofSystem.SECURITY_COUNCIL.v1");

    /// @notice The signature threshold required for a valid council attestation.
    uint256 public immutable threshold;

    /// @notice Whether an address is a council member.
    mapping(address member => bool isMember) public isMember;

    /// @notice Thrown when fewer than `threshold` valid signatures are supplied.
    error InsufficientSignatures(uint256 provided, uint256 required);

    /// @notice Thrown when a signature is malformed.
    error MalformedSignature();

    /// @notice Thrown when signatures are not strictly ascending by signer
    ///         address (used to reject duplicate signers cheaply).
    error UnorderedSigners();

    /// @notice Thrown when a recovered signer is not a council member.
    error NotCouncilMember(address signer);

    /// @param members_ The initial council member set.
    /// @param threshold_ The required number of distinct signatures.
    constructor(address[] memory members_, uint256 threshold_) {
        for (uint256 i = 0; i < members_.length; i++) {
            isMember[members_[i]] = true;
        }
        threshold = threshold_;
    }

    /// @inheritdoc IWorldChainProofVerifier
    /// @dev `proof` is `abi.encode(bytes[] signatures)` over the digest
    ///      `keccak256(SECURITY_COUNCIL_DOMAIN, rootId)`. Signatures MUST be
    ///      ordered by ascending recovered signer address to guarantee
    ///      distinctness.
    function verify(bytes32 rootId, bytes calldata proof) external view returns (bool) {
        bytes[] memory signatures = abi.decode(proof, (bytes[]));
        if (signatures.length < threshold) revert InsufficientSignatures(signatures.length, threshold);

        bytes32 digest = keccak256(abi.encode(SECURITY_COUNCIL_DOMAIN, rootId));

        address last = address(0);
        for (uint256 i = 0; i < signatures.length; i++) {
            bytes memory signature = signatures[i];
            if (signature.length != 65) revert MalformedSignature();

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
            if (signer <= last) revert UnorderedSigners();
            if (!isMember[signer]) revert NotCouncilMember(signer);
            last = signer;
        }
        return true;
    }
}
