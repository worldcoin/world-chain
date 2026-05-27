// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

enum RootState {
    NONE,
    PROPOSED,
    CHALLENGED,
    FINALIZED,
    INVALIDATED
}

enum ProofLane {
    VALIDITY_PROOF,
    FAULT_PROOF_GAME,
    TEE_ATTESTATION,
    SECURITY_COUNCIL
}

struct DomainConfig {
    uint256 chainId;
    uint256 proofSystemVersion;
    bytes32 rollupConfigHash;
    uint256 blockInterval;
    uint256 intermediateBlockInterval;
}

struct RootCommitment {
    bytes32 rootClaim;
    uint256 l2BlockNumber;
    address parentRef;
    bytes32 intermediateRootsHash;
    bytes32 l1OriginHash;
    uint256 l1OriginNumber;
}
