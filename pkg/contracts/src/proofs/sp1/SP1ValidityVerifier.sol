// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "../interfaces/IWorldChainProofVerifier.sol";
import {ISP1Verifier} from "@sp1-contracts/src/ISP1Verifier.sol";
import {ProofLib} from "../lib/ProofLib.sol";
import {ProofVerificationLib} from "../lib/ProofVerificationLib.sol";

/// Must match `world_chain_proof_core::types::AggregationPublicValues`.
struct AggregationPublicValues {
    ProofLib.TransitionPublicValues transitionPublicValues;
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
///      `MultiProofGame`.
contract SP1ValidityVerifier is IWorldChainProofVerifier {
    /*//////////////////////////////////////////////////////////////
                                ERRORS
    //////////////////////////////////////////////////////////////*/

    /// @notice Thrown when the SP1 verifier gateway address is zero.
    error ZeroSP1Verifier();

    /// @notice Thrown when the aggregation program verification key is zero.
    error ZeroAggregationVKey();

    /// @notice Thrown when the expected range program verification key is zero.
    error ZeroRangeVKeyCommitment();

    /*//////////////////////////////////////////////////////////////
                               STORAGE
    //////////////////////////////////////////////////////////////*/

    /// @notice Succinct SP1 verifier gateway or verifier implementation.
    ISP1Verifier public immutable sp1Verifier;

    /// @notice Verification key for the World Chain aggregation program.
    bytes32 public immutable aggregationVKey;

    /// @notice Range-program verification key committed by the aggregation proof.
    bytes32 public immutable rangeVKeyCommitment;

    /*//////////////////////////////////////////////////////////////
                             CONSTRUCTOR
    //////////////////////////////////////////////////////////////*/

    constructor(ISP1Verifier sp1Verifier_, bytes32 aggregationVKey_, bytes32 rangeVKeyCommitment_) {
        if (address(sp1Verifier_) == address(0)) revert ZeroSP1Verifier();
        if (aggregationVKey_ == bytes32(0)) revert ZeroAggregationVKey();
        if (rangeVKeyCommitment_ == bytes32(0)) revert ZeroRangeVKeyCommitment();

        sp1Verifier = sp1Verifier_;
        aggregationVKey = aggregationVKey_;
        rangeVKeyCommitment = rangeVKeyCommitment_;
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
    ///      Verification runs in two stages so the failure class survives.
    ///      Stage one decodes and binds behind an external `this.` call, so any
    ///      revert is unambiguously a malformed payload. Stage two calls the
    ///      SP1 gateway directly, distinguishing a judged rejection from a
    ///      gateway that never produced a judgement at all.
    function verify(bytes32 rootId, bytes calldata proof) external view returns (ProofLib.VerificationStatus) {
        ProofLib.VerificationStatus binding;
        bytes memory publicValues;
        bytes memory proofBytes;
        try this._decodeAndBind(msg.sender, rootId, proof) returns (
            ProofLib.VerificationStatus binding_, bytes memory publicValues_, bytes memory proofBytes_
        ) {
            binding = binding_;
            publicValues = publicValues_;
            proofBytes = proofBytes_;
        } catch {
            return ProofLib.VerificationStatus.MALFORMED;
        }
        if (binding != ProofLib.VerificationStatus.VALID) return binding;

        // A gateway with no code cannot judge the proof. Without this the `extcodesize`-free
        // staticcall below would succeed with empty returndata and read as a valid proof.
        if (address(sp1Verifier).code.length == 0) return ProofLib.VerificationStatus.UNAVAILABLE;

        try sp1Verifier.verifyProof(aggregationVKey, publicValues, proofBytes) {
            return ProofLib.VerificationStatus.VALID;
        } catch (bytes memory reason) {
            // Empty returndata means no judgement was reached — an out-of-gas under the EIP-150
            // 63/64 rule, a bare `revert()`, or a gateway with no route for this proof kind.
            // Reporting that as REJECTED would send an operator off to regenerate a proof that
            // was never actually found wanting.
            return reason.length == 0 ? ProofLib.VerificationStatus.UNAVAILABLE : ProofLib.VerificationStatus.REJECTED;
        }
    }

    /// @notice External helper used only by `verify`; MUST NOT be called
    ///         directly.
    /// @dev External so `verify` can catch ABI decode failures. Performs no
    ///      cryptography — it only decodes and binds the payload to the game.
    function _decodeAndBind(address gameAddress, bytes32 rootId, bytes calldata proof)
        external
        view
        returns (ProofLib.VerificationStatus status, bytes memory publicValues, bytes memory proofBytes)
    {
        require(msg.sender == address(this), "internal");

        bytes32 domainHash;
        address parentRef;
        uint256 l1OriginNumber;
        (domainHash, parentRef, l1OriginNumber, publicValues, proofBytes) =
            abi.decode(proof, (bytes32, address, uint256, bytes, bytes));

        AggregationPublicValues memory outputs = abi.decode(publicValues, (AggregationPublicValues));
        ProofLib.TransitionPublicValues memory transition = outputs.transitionPublicValues;

        // A proof from a different range program is bound to the wrong domain, not cryptographically
        // unsound, so it is a binding mismatch rather than a rejection.
        if (outputs.multiBlockVKey != rangeVKeyCommitment) {
            return (ProofLib.VerificationStatus.BINDING_MISMATCH, publicValues, proofBytes);
        }

        status =
            ProofVerificationLib.matchesGame(gameAddress, rootId, domainHash, parentRef, l1OriginNumber, transition);
    }
}
