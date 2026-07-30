// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

library ProofLib {
    /// Default number of distinct proof lanes required to finalize a challenged
    /// root. Deployments may override this per game implementation (see
    /// `IMultiProofGame.GameConfig.proofThreshold`).
    uint8 internal constant PROOF_THRESHOLD = 2;
    uint8 internal constant PROOF_LANE_COUNT = 3;

    enum RootState {
        /// @dev Never returned by `MultiProofGame.state()`; a clone always exists in one of the
        ///      states below. Retained so the zero value is not a meaningful state, and so the
        ///      ordinals match `world_chain_proofs::RootState`.
        NONE,
        PROPOSED,
        CHALLENGED,
        FINALIZED,
        INVALIDATED
    }

    enum InvalidationReason {
        NONE,
        PROOF_TIMEOUT,
        /// @dev Also covers a blacklisted parent: blacklisting an ancestor invalidates its
        ///      descendants through the same cascade, so they report `INVALID_PARENT`.
        INVALID_PARENT,
        /// @dev Never set by `MultiProofGame`. Blacklisting a game does not change its own
        ///      `GameStatus`; it makes the game improper, which the registry enforces and which
        ///      the game reflects by settling bonds in `REFUND` mode. Retained so the ordinals
        ///      match `world_chain_proofs::InvalidationReason`.
        BLACKLISTED
    }

    enum ProofLane {
        VALIDITY_PROOF,
        TEE_ATTESTATION,
        SECURITY_COUNCIL
    }

    /// Outcome of a lane verification, carrying the failure class so callers can distinguish a
    /// submitter error from a dependency outage. A bare boolean collapses "this proof is wrong"
    /// into "the verifier gateway is down", which are opposite operational responses during a
    /// live proof window.
    enum VerificationStatus {
        /// The proof verified against the game's transition.
        VALID,
        /// The proof payload could not be ABI-decoded into the lane's expected layout.
        MALFORMED,
        /// The payload decoded but does not bind to this game's rootId, domain, or transition.
        BINDING_MISMATCH,
        /// The proof bound correctly but failed its cryptographic check.
        REJECTED,
        /// A dependency (key registry, verifier gateway) failed. The proof is unjudged; retry.
        UNAVAILABLE
    }

    struct Domain {
        uint256 chainId;
        uint256 proofSystemVersion;
        bytes32 rollupConfigHash;
        uint256 blockInterval;
    }

    /// ABI-encoded public values shared by all transition proof lanes.
    /// Must match `world_chain_proof_core::boot::TransitionPublicValues`.
    struct TransitionPublicValues {
        bytes32 l1Head;
        bytes32 l2PreRoot;
        uint64 l2PreBlockNumber;
        bytes32 l2PostRoot;
        uint64 l2PostBlockNumber;
        bytes32 rollupConfigHash;
    }

    function domainHash(Domain memory domain) internal pure returns (bytes32) {
        return
            keccak256(
                abi.encode(domain.chainId, domain.proofSystemVersion, domain.rollupConfigHash, domain.blockInterval)
            );
    }

    /// @dev `parentRef` alone does not pin the pre-state. For a concrete parent game the address
    ///      transitively determines its root, but for the anchor-registry sentinel the address is
    ///      fixed while the anchor value moves, so two proposals reading different anchor roots
    ///      would otherwise share a rootId. The pre-state is therefore committed explicitly, and
    ///      every lane binds to it without having to read the game back.
    function rootId(
        bytes32 domainHash_,
        address parentRef,
        bytes32 startingRootClaim,
        uint256 startingL2BlockNumber,
        bytes32 rootClaim,
        uint256 l2BlockNumber,
        bytes32 l1OriginHash,
        uint256 l1OriginNumber
    ) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                domainHash_,
                parentRef,
                startingRootClaim,
                startingL2BlockNumber,
                rootClaim,
                l2BlockNumber,
                l1OriginHash,
                l1OriginNumber
            )
        );
    }

    function laneMask(ProofLane lane) internal pure returns (uint8) {
        return uint8(1) << uint8(lane);
    }

    function proofCount(uint8 bitmap) internal pure returns (uint8 count) {
        for (uint8 i = 0; i < PROOF_LANE_COUNT; i++) {
            if ((bitmap & (uint8(1) << i)) != 0) {
                count++;
            }
        }
    }

    function hasThreshold(uint8 bitmap, uint8 threshold) internal pure returns (bool) {
        return proofCount(bitmap) >= threshold;
    }
}
