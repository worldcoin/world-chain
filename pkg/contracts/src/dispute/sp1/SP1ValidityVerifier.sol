// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "../interfaces/IWorldChainProofVerifier.sol";
import {ISP1Verifier} from "@sp1-contracts/src/ISP1Verifier.sol";
import {LibProof, TransitionPublicValues} from "../lib/LibProof.sol";

/// Must match `world_chain_proof_core::types::AggregationPublicValues`.
struct AggregationPublicValues {
    TransitionPublicValues transitionPublicValues;
    bytes32 multiBlockVKey;
}

/// @title SP1ValidityVerifier
/// @author World Contributors
/// @custom:security-contact security@toolsforhumanity.com
contract SP1ValidityVerifier is IWorldChainProofVerifier {
    /// @notice Thrown when the SP1 verifier gateway address is zero.
    error ZeroSP1Verifier();

    /// @notice Thrown when the aggregation program verification key is zero.
    error ZeroAggregationVKey();

    /// @notice Thrown when the expected range program verification key is zero.
    error ZeroRangeVKeyCommitment();

    /// @notice Succinct SP1 verifier gateway or verifier implementation.
    ISP1Verifier public immutable sp1Verifier;

    /// @notice Verification key for the World Chain aggregation program.
    bytes32 public immutable aggregationVKey;

    /// @notice Range-program verification key committed by the aggregation proof.
    bytes32 public immutable rangeVKeyCommitment;

    constructor(ISP1Verifier sp1Verifier_, bytes32 aggregationVKey_, bytes32 rangeVKeyCommitment_) {
        if (address(sp1Verifier_) == address(0)) revert ZeroSP1Verifier();
        if (aggregationVKey_ == bytes32(0)) revert ZeroAggregationVKey();
        if (rangeVKeyCommitment_ == bytes32(0)) revert ZeroRangeVKeyCommitment();

        sp1Verifier = sp1Verifier_;
        aggregationVKey = aggregationVKey_;
        rangeVKeyCommitment = rangeVKeyCommitment_;
    }

    /// @inheritdoc IWorldChainProofVerifier
    /// @dev `proof` is the SP1 on-chain proof payload; for gateway deployments its first four
    ///      bytes select the concrete verifier route. The public values are reconstructed from
    ///      the game-supplied transition and this verifier's range-program commitment, so the
    ///      proof can only verify if it attests exactly that transition. Invalid or malformed
    ///      proofs revert inside the gateway and surface as `false`.
    function verify(bytes32, TransitionPublicValues calldata transition, bytes calldata proof)
        external
        view
        returns (bool)
    {
        bytes memory publicValues = abi.encode(
            AggregationPublicValues({transitionPublicValues: transition, multiBlockVKey: rangeVKeyCommitment})
        );
        try sp1Verifier.verifyProof(aggregationVKey, publicValues, proof) {
            return true;
        } catch {
            return false;
        }
    }
}
