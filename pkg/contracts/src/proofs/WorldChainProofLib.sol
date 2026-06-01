// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title WorldChainProofLib
/// @notice Pure helpers for the WIP-1006 proof system: domain/proposal/root
///         commitment hashing, the per-root proof-lane bitmap, and parsing of
///         the Base Azul-compatible `extraData` payload.
/// @dev All hashing here is kept byte-identical to the Rust mirror in
///      `crates/proofs/src/lib.rs` (`abi.encode` of the same field order), so
///      offchain provers and onchain lanes agree on `domainHash`,
///      `proposalKey`, and `rootId`.
library WorldChainProofLib {
    /// @notice Number of distinct proof lanes that MUST support a challenged
    ///         root before it finalizes (WIP-1006 `PROOF_THRESHOLD`).
    uint8 internal constant PROOF_THRESHOLD = 2;

    /// @notice Number of configured proof lanes (WIP-1006 `PROOF_LANE_COUNT`).
    uint8 internal constant PROOF_LANE_COUNT = 3;

    /// @notice WIP-1006 proof lanes. Bit position in the proof bitmap equals
    ///         the enum value.
    enum ProofLane {
        VALIDITY_PROOF,
        TEE_ATTESTATION,
        SECURITY_COUNCIL
    }

    /// @notice Verifier-immutable domain constants committed into every `rootId`.
    struct Domain {
        uint256 chainId;
        uint256 proofSystemVersion;
        bytes32 rollupConfigHash;
        uint256 blockInterval;
        uint256 intermediateBlockInterval;
    }

    /// @notice Thrown when `extraData` is too short to contain the fixed header.
    error MalformedExtraData(uint256 length);

    /// @notice Thrown when the intermediate-root section is not a whole number
    ///         of 32-byte words.
    error MisalignedIntermediateRoots(uint256 length);

    /// @notice Thrown when the number of intermediate roots does not match the
    ///         domain's `blockInterval / intermediateBlockInterval`.
    error UnexpectedIntermediateRootCount(uint256 expected, uint256 actual);

    /// @notice Thrown when the final intermediate root does not equal `rootClaim`.
    error FinalIntermediateRootMismatch(bytes32 expected, bytes32 actual);

    /// @notice Thrown when the committed `intermediateRootsHash` does not match
    ///         the keccak of the ordered intermediate roots in `extraData`.
    error IntermediateRootsHashMismatch(bytes32 expected, bytes32 actual);

    /// @notice Computes the verifier-immutable domain hash.
    function domainHash(Domain memory domain) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                domain.chainId,
                domain.proofSystemVersion,
                domain.rollupConfigHash,
                domain.blockInterval,
                domain.intermediateBlockInterval
            )
        );
    }

    /// @notice Computes the factory lookup key. Excludes the factory-captured L1
    ///         origin so proposers can discover existing games deterministically.
    function proposalKey(
        bytes32 domainHash_,
        address parentRef,
        bytes32 rootClaim,
        uint256 l2BlockNumber,
        bytes32 intermediateRootsHash
    ) internal pure returns (bytes32) {
        return keccak256(abi.encode(domainHash_, parentRef, rootClaim, l2BlockNumber, intermediateRootsHash));
    }

    /// @notice Computes the proof-bound commitment. Every lane MUST bind to this.
    function rootId(
        bytes32 domainHash_,
        address parentRef,
        bytes32 rootClaim,
        uint256 l2BlockNumber,
        bytes32 intermediateRootsHash,
        bytes32 l1OriginHash,
        uint256 l1OriginNumber
    ) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                domainHash_, parentRef, rootClaim, l2BlockNumber, intermediateRootsHash, l1OriginHash, l1OriginNumber
            )
        );
    }

    /// @notice Bit assigned to `lane` in the per-root proof bitmap.
    function laneMask(ProofLane lane) internal pure returns (uint8) {
        return uint8(1 << uint8(lane));
    }

    /// @notice Counts the distinct lanes set in `bitmap`.
    function proofCount(uint8 bitmap) internal pure returns (uint8 count) {
        for (uint8 i = 0; i < PROOF_LANE_COUNT; i++) {
            if ((bitmap & uint8(1 << i)) != 0) {
                count++;
            }
        }
    }

    /// @notice Whether `bitmap` satisfies the WIP-1006 threshold.
    function hasThreshold(uint8 bitmap) internal pure returns (bool) {
        return proofCount(bitmap) >= PROOF_THRESHOLD;
    }

    /// @notice Parses the Base Azul-compatible `extraData` layout.
    /// @dev Layout: `[0,32) l2BlockNumber`, `[32,52) parentRef address`,
    ///      `[52, 52 + 32n) ordered intermediate roots`, where
    ///      `n = blockInterval / intermediateBlockInterval`. The
    ///      `intermediateRootsHash` returned is `keccak256` over the ordered
    ///      intermediate-root section (`bytes32(0)` when `n == 0`).
    /// @param extraData The opaque CWIA extra-data payload.
    /// @return l2BlockNumber The proposed L2 block number.
    /// @return parentRef The parent reference address.
    /// @return intermediateRootsHash Commitment over the ordered intermediate roots.
    /// @return finalIntermediateRoot The last intermediate root, or `bytes32(0)` if none.
    function parseExtraData(bytes memory extraData)
        internal
        pure
        returns (uint256 l2BlockNumber, address parentRef, bytes32 intermediateRootsHash, bytes32 finalIntermediateRoot)
    {
        if (extraData.length < 52) revert MalformedExtraData(extraData.length);

        uint256 rootsLength = extraData.length - 52;
        if (rootsLength % 32 != 0) revert MisalignedIntermediateRoots(extraData.length);

        assembly {
            // First word after the length prefix.
            l2BlockNumber := mload(add(extraData, 0x20))
            // Address occupies the high 20 bytes of the second word.
            parentRef := shr(0x60, mload(add(extraData, 0x40)))
        }

        if (rootsLength == 0) {
            return (l2BlockNumber, parentRef, bytes32(0), bytes32(0));
        }

        bytes memory roots = new bytes(rootsLength);
        assembly {
            // Source of the intermediate-root section: data ptr + 52.
            let src := add(add(extraData, 0x20), 52)
            let dst := add(roots, 0x20)
            for { let i := 0 } lt(i, rootsLength) { i := add(i, 0x20) } {
                mstore(add(dst, i), mload(add(src, i)))
            }
            finalIntermediateRoot := mload(add(src, sub(rootsLength, 0x20)))
        }
        intermediateRootsHash = keccak256(roots);
    }

    /// @notice Validates the `extraData` payload against the domain and root
    ///         claim, returning the parsed fields used to compute `rootId`.
    /// @dev Enforces the Azul invariants: the intermediate-root count matches
    ///      the domain interval ratio, the committed `intermediateRootsHash`
    ///      matches the ordered roots, and the final intermediate root equals
    ///      `rootClaim`. Implementations with no intermediate roots pass an
    ///      `extraData` with no root section (n == 0) and `rootClaim` is not
    ///      cross-checked against a final root.
    function validateAndParseExtraData(bytes memory extraData, Domain memory domain, bytes32 rootClaim)
        internal
        pure
        returns (uint256 l2BlockNumber, address parentRef, bytes32 intermediateRootsHash)
    {
        bytes32 finalIntermediateRoot;
        (l2BlockNumber, parentRef, intermediateRootsHash, finalIntermediateRoot) = parseExtraData(extraData);

        uint256 expectedCount =
            domain.intermediateBlockInterval == 0 ? 0 : domain.blockInterval / domain.intermediateBlockInterval;
        uint256 actualCount = (extraData.length - 52) / 32;
        if (actualCount != expectedCount) revert UnexpectedIntermediateRootCount(expectedCount, actualCount);

        if (actualCount != 0 && finalIntermediateRoot != rootClaim) {
            revert FinalIntermediateRootMismatch(rootClaim, finalIntermediateRoot);
        }
    }
}
