// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "../interfaces/IWorldChainProofVerifier.sol";
import {ISP1Verifier} from "@sp1-contracts/src/ISP1Verifier.sol";
import {WorldChainProofLib} from "../WorldChainProofLib.sol";
import {WorldChainProofVerificationLib} from "../WorldChainProofVerificationLib.sol";

/// Must match `world_chain_proof_core::types::AggregationPublicValues`.
struct AggregationPublicValues {
    WorldChainProofLib.TransitionPublicValues transitionPublicValues;
    bytes32 multiBlockVKey;
}

/// @title SP1ValidityVerifier
/// @author World Contributors
/// @notice SP1 validity-proof lane verifier compatible with WIP-1006's
///         multi-proof system (`IWorldChainProofVerifier`).
/// @dev The verifier checks the SP1 aggregation proof with Succinct's verifier
///      gateway, then binds the aggregation public values to the supplied
///      World Chain `rootId`. Invalid proofs return `false` rather than
///      bubbling reverts, matching the predicate contract expected by
///      `WorldChainProofSystemGame`.
contract SP1ValidityVerifier is IWorldChainProofVerifier {
    /*//////////////////////////////////////////////////////////////
                                ERRORS
    //////////////////////////////////////////////////////////////*/

    /// @notice Thrown when the SP1 verifier gateway address is zero.
    error ZeroSP1Verifier();

    /// @notice Thrown when the aggregation program verification key is zero.
    error ZeroAggregationVKey();

    /// @notice Thrown when the expected rollup config hash is zero.
    error ZeroRollupConfigHash();

    /// @notice Thrown when the expected range program verification key is zero.
    error ZeroRangeVKeyCommitment();

    /// @notice Thrown when the anchor-state registry address is zero.
    error ZeroAnchorStateRegistry();

    /*//////////////////////////////////////////////////////////////
                               STORAGE
    //////////////////////////////////////////////////////////////*/

    /// @notice Succinct SP1 verifier gateway or verifier implementation.
    ISP1Verifier public immutable sp1Verifier;

    /// @notice Verification key for the World Chain aggregation program.
    bytes32 public immutable aggregationVKey;

    /// @notice Rollup config hash the aggregation public values must commit to.
    bytes32 public immutable rollupConfigHash;

    /// @notice Range-program verification key committed by the aggregation proof.
    bytes32 public immutable rangeVKeyCommitment;

    /// @notice Anchor-state registry that calling games must belong to.
    address public immutable anchorStateRegistry;

    /*//////////////////////////////////////////////////////////////
                             CONSTRUCTOR
    //////////////////////////////////////////////////////////////*/

    constructor(
        ISP1Verifier sp1Verifier_,
        bytes32 aggregationVKey_,
        bytes32 rollupConfigHash_,
        bytes32 rangeVKeyCommitment_,
        address anchorStateRegistry_
    ) {
        if (address(sp1Verifier_) == address(0)) revert ZeroSP1Verifier();
        if (aggregationVKey_ == bytes32(0)) revert ZeroAggregationVKey();
        if (rollupConfigHash_ == bytes32(0)) revert ZeroRollupConfigHash();
        if (rangeVKeyCommitment_ == bytes32(0)) revert ZeroRangeVKeyCommitment();
        if (anchorStateRegistry_ == address(0)) revert ZeroAnchorStateRegistry();

        sp1Verifier = sp1Verifier_;
        aggregationVKey = aggregationVKey_;
        rollupConfigHash = rollupConfigHash_;
        rangeVKeyCommitment = rangeVKeyCommitment_;
        anchorStateRegistry = anchorStateRegistry_;
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
    ///            uint256 l1OriginNumber,
    ///            bytes   publicValues,
    ///            bytes   proofBytes
    ///        )
    ///
    ///      `publicValues` must be `abi.encode(AggregationPublicValues)`.
    ///      `proofBytes` is the SP1 on-chain proof payload; for gateway
    ///      deployments its first four bytes select the concrete verifier route.
    ///
    ///      Decoding and verification live behind an external `this.` call so
    ///      the try/catch in `verify` traps malformed ABI payloads, invalid
    ///      public values, and SP1 verifier reverts as `false`.
    function verify(bytes32 rootId, bytes calldata proof) external view returns (bool) {
        try this._decodeAndVerify(msg.sender, rootId, proof) returns (bool ok) {
            return ok;
        } catch {
            return false;
        }
    }

    /// @notice External helper used only by `verify`; MUST NOT be called
    ///         directly.
    /// @dev External so `verify` can catch every revert path, including ABI
    ///      decode failures and verifier-gateway reverts.
    function _decodeAndVerify(address gameAddress, bytes32 rootId, bytes calldata proof) external view returns (bool) {
        require(msg.sender == address(this), "internal");

        (
            bytes32 domainHash,
            address parentRef,
            uint256 l1OriginNumber,
            bytes memory publicValues,
            bytes memory proofBytes
        ) = abi.decode(proof, (bytes32, address, uint256, bytes, bytes));

        AggregationPublicValues memory outputs = abi.decode(publicValues, (AggregationPublicValues));
        WorldChainProofLib.TransitionPublicValues memory transition = outputs.transitionPublicValues;

        if (transition.rollupConfigHash != rollupConfigHash) return false;
        if (outputs.multiBlockVKey != rangeVKeyCommitment) return false;

        bool matchesGame = WorldChainProofVerificationLib.matchesGame(
            gameAddress, anchorStateRegistry, rootId, domainHash, parentRef, l1OriginNumber, transition
        );
        if (!matchesGame) return false;

        sp1Verifier.verifyProof(aggregationVKey, publicValues, proofBytes);
        return true;
    }
}
