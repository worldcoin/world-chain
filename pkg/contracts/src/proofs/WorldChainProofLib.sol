// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

library WorldChainProofLib {
    /// Default number of distinct proof lanes required to finalize a challenged
    /// root. Deployments may override this per-factory (see
    /// `WorldChainProofSystemFactory`'s `proofThreshold`).
    uint8 internal constant PROOF_THRESHOLD = 2;
    uint8 internal constant PROOF_LANE_COUNT = 3;

    enum RootState {
        NONE,
        PROPOSED,
        CHALLENGED,
        FINALIZED,
        INVALIDATED
    }

    enum InvalidationReason {
        NONE,
        PROOF_TIMEOUT,
        INVALID_PARENT,
        BLACKLISTED
    }

    enum ProofLane {
        VALIDITY_PROOF,
        TEE_ATTESTATION,
        SECURITY_COUNCIL
    }

    struct Domain {
        uint256 chainId;
        uint256 proofSystemVersion;
        bytes32 rollupConfigHash;
        uint256 blockInterval;
    }

    function domainHash(Domain memory domain) internal pure returns (bytes32) {
        return
            keccak256(
                abi.encode(domain.chainId, domain.proofSystemVersion, domain.rollupConfigHash, domain.blockInterval)
            );
    }

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

    function proposalKey(bytes32 domainHash_, address parentRef, bytes32 rootClaim, uint256 l2BlockNumber)
        internal
        pure
        returns (bytes32)
    {
        return keccak256(abi.encode(domainHash_, parentRef, rootClaim, l2BlockNumber));
    }

    function laneMask(ProofLane lane) internal pure returns (uint8) {
        return uint8(1 << uint8(lane));
    }

    function proofCount(uint8 bitmap) internal pure returns (uint8 count) {
        for (uint8 i = 0; i < PROOF_LANE_COUNT; i++) {
            if ((bitmap & (1 << i)) != 0) {
                count++;
            }
        }
    }

    function hasThreshold(uint8 bitmap, uint8 threshold) internal pure returns (bool) {
        return proofCount(bitmap) >= threshold;
    }
}
