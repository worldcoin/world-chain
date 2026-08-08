// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "../interfaces/IWorldChainProofVerifier.sol";

/// The set of proof lanes accepted for a proposal, one bit per `ProofLane`.
type Bitmap is uint8;

enum ProofLane {
    VALIDITY_PROOF,
    TEE_ATTESTATION,
    SECURITY_COUNCIL
}

enum InvalidationReason {
    NONE,
    PROOF_TIMEOUT,
    INVALID_PARENT
}

/// A decoded `submitProofLane` payload.
/// @param laneId The `ProofLane` the payload proves.
/// @param recipient Earns this lane's share of a forfeited challenger bond.
/// @param proof The lane-specific proof bytes passed to the lane's verifier.
struct CompactProof {
    uint8 laneId;
    address recipient;
    bytes proof;
}

/// @dev ABI-encoded public values shared by all transition proof lanes.
struct TransitionPublicValues {
    bytes32 l1Head;
    bytes32 l2PreRoot;
    uint64 l2PreBlockNumber;
    bytes32 l2PostRoot;
    uint64 l2PostBlockNumber;
    bytes32 rollupConfigHash;
}

/// @title LibProof
/// @author World Contributors
/// @custom:security-contact security@toolsforhumanity.com
library LibProof {
    /// @dev Quantifies the number of proof lanes in the MultiProofGame.
    uint8 internal constant PROOF_LANE_COUNT = 3;

    /// @dev Version of the proof-domain encoding.
    uint256 internal constant PROOF_SYSTEM_VERSION = 1;

    /// @dev Lane id at byte 0, reward recipient at bytes 1..20, proof payload after.
    uint256 internal constant PROOF_HEADER_LENGTH = 21;

    /// @dev Commitment binding a deployment to its chain, proof-system version, rollup
    ///      configuration, and proposal cadence.
    function domainHash(uint256 chainId, uint256 proofSystemVersion, bytes32 rollupConfigHash, uint256 blockInterval)
        internal
        pure
        returns (bytes32)
    {
        return keccak256(abi.encode(chainId, proofSystemVersion, rollupConfigHash, blockInterval));
    }

    /// @dev Canonical proposal identity used by the game and council attestation.
    function rootId(
        bytes32 domainHash_,
        address parentRef,
        bytes32 rootClaim,
        uint256 l2BlockNumber,
        bytes32 l1OriginHash,
        uint256 l1OriginNumber
    ) internal pure returns (bytes32) {
        return keccak256(abi.encode(domainHash_, parentRef, rootClaim, l2BlockNumber, l1OriginHash, l1OriginNumber));
    }

    /// @dev Selects the verifier backing `lane`.
    function verifierFor(
        ProofLane lane,
        IWorldChainProofVerifier validityProofVerifier,
        IWorldChainProofVerifier teeVerifier,
        IWorldChainProofVerifier securityCouncil
    ) internal pure returns (IWorldChainProofVerifier) {
        if (lane == ProofLane.VALIDITY_PROOF) return validityProofVerifier;
        if (lane == ProofLane.TEE_ATTESTATION) return teeVerifier;
        return securityCouncil;
    }

    /// @dev Selects the immutable verifier identity backing `lane`.
    function verifierIdFor(ProofLane lane, bytes32 aggregationVKey, bytes32 teeImageId)
        internal
        pure
        returns (bytes32)
    {
        if (lane == ProofLane.VALIDITY_PROOF) return aggregationVKey;
        if (lane == ProofLane.TEE_ATTESTATION) return teeImageId;
        return bytes32(0);
    }

    /// @dev Encodes the exact public values authenticated by `lane`.
    ///      The validity and TEE lanes bind the transition, not `rootId` or a separate upgrade
    ///      schedule. A derivation upgrade must therefore rotate at least one bound value:
    ///      `rollupConfigHash`, the relevant SP1 vkey, or the Nitro PCR0 image ID.
    function publicValuesFor(
        ProofLane lane,
        bytes32 rootId_,
        TransitionPublicValues memory transition,
        bytes32 rangeVKeyCommitment
    ) internal pure returns (bytes memory) {
        if (lane == ProofLane.VALIDITY_PROOF) {
            return abi.encode(transition, rangeVKeyCommitment);
        }
        if (lane == ProofLane.TEE_ATTESTATION) return abi.encode(transition);
        return abi.encode(rootId_);
    }

    /// @dev Splits a compact payload into its lane id, reward recipient, and verifier proof
    ///      bytes. Callers must reject `compact.length < PROOF_HEADER_LENGTH` first.
    function decodeCompact(bytes calldata compact) internal pure returns (CompactProof memory decoded) {
        assembly ("memory-safe") {
            // word = [ laneId (1) | recipient (20) | 11 bytes ignored ]
            let header := calldataload(compact.offset)
            mstore(decoded, byte(0, header))
            mstore(add(decoded, 0x20), shr(96, shl(8, header)))

            let length := sub(compact.length, PROOF_HEADER_LENGTH)
            let payload := mload(0x40)
            mstore(payload, length)
            calldatacopy(add(payload, 0x20), add(compact.offset, PROOF_HEADER_LENGTH), length)
            // Bump past the length word plus the payload rounded up to a whole word.
            mstore(0x40, add(payload, and(add(length, 0x3f), not(0x1f))))
            mstore(add(decoded, 0x40), payload)
        }
    }

    /// @dev The underlying bits of `bitmap`.
    function raw(Bitmap bitmap) internal pure returns (uint8) {
        return Bitmap.unwrap(bitmap);
    }

    /// @dev The single-bit bitmap representing `lane`.
    function mask(ProofLane lane) internal pure returns (Bitmap) {
        return Bitmap.wrap(uint8(1) << uint8(lane));
    }

    /// @dev Whether `lane` has already been accepted.
    function has(Bitmap bitmap, ProofLane lane) internal pure returns (bool) {
        return bitmap.raw() & lane.mask().raw() != 0;
    }

    /// @dev `bitmap` with `lane` accepted.
    function set(Bitmap bitmap, ProofLane lane) internal pure returns (Bitmap) {
        return Bitmap.wrap(bitmap.raw() | lane.mask().raw());
    }

    /// @dev Accepted lane count; bits above `PROOF_LANE_COUNT` cannot inflate it.
    function count(Bitmap bitmap) internal pure returns (uint8 accepted) {
        for (uint8 laneId = 0; laneId < PROOF_LANE_COUNT; laneId++) {
            if (bitmap.has(ProofLane(laneId))) {
                accepted++;
            }
        }
    }

    /// @dev Whether enough distinct lanes have been accepted to prove a challenged proposal.
    function hasThreshold(Bitmap bitmap, uint8 threshold) internal pure returns (bool) {
        return bitmap.count() >= threshold;
    }
}

using LibProof for Bitmap global;
using LibProof for ProofLane global;
