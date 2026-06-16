// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "./interfaces/IWorldChainProofVerifier.sol";
import {ISP1Verifier} from "@sp1-contracts/src/ISP1Verifier.sol";

/// Minimal view of the game contract that calls this verifier. The game exposes
/// the proposal's committed fields as public immutables; the verifier reads them
/// via `msg.sender` so the proof calldata only needs to carry the SP1 payload.
interface IWorldChainProofSystemGameView {
    function rootClaim() external view returns (bytes32);
    function l2BlockNumber() external view returns (uint256);
    function l1OriginHash() external view returns (bytes32);
}

/// ABI-encoded public values committed by the SP1 aggregation program. Must match
/// `world_chain_proof_core::types::AggregationOutputs` field-for-field.
struct AggregationOutputs {
    bytes32 l1Head;
    bytes32 l2PreRoot;
    bytes32 l2PostRoot;
    uint64 l2BlockNumber;
    bytes32 rollupConfigHash;
    bytes32 multiBlockVKey;
    address proverAddress;
}

/// @title SP1 validity-proof verifier for the World Chain proof system.
/// @notice Verifies an SP1 Groth16 aggregation proof and binds its committed
/// outputs to the challenged game's proposal. Implements the validity-proof lane
/// of [`IWorldChainProofVerifier`].
contract SP1ValidityVerifier is IWorldChainProofVerifier {
    /// The Succinct SP1 Groth16 verifier (version-matched to the proving stack).
    ISP1Verifier public immutable sp1Verifier;
    /// Verification key of the aggregation program (`agg_pk.verifying_key().bytes32()`).
    bytes32 public immutable aggregationVKey;
    /// Rollup config hash the proof must commit to (pins the chain).
    bytes32 public immutable rollupConfigHash;
    /// Range-program vkey commitment the aggregation proof must commit to.
    bytes32 public immutable rangeVKeyCommitment;

    error RootClaimMismatch(bytes32 expected, bytes32 actual);
    error BlockNumberMismatch(uint256 expected, uint64 actual);
    error L1HeadMismatch(bytes32 expected, bytes32 actual);
    error RollupConfigHashMismatch(bytes32 expected, bytes32 actual);
    error RangeVKeyMismatch(bytes32 expected, bytes32 actual);

    constructor(
        ISP1Verifier sp1Verifier_,
        bytes32 aggregationVKey_,
        bytes32 rollupConfigHash_,
        bytes32 rangeVKeyCommitment_
    ) {
        sp1Verifier = sp1Verifier_;
        aggregationVKey = aggregationVKey_;
        rollupConfigHash = rollupConfigHash_;
        rangeVKeyCommitment = rangeVKeyCommitment_;
    }

    /// @inheritdoc IWorldChainProofVerifier
    /// @param proof `abi.encode(bytes publicValues, bytes proofBytes)`, where
    /// `publicValues == abi.encode(AggregationOutputs)` and `proofBytes` is the SP1
    /// Groth16 proof. The `rootId` argument is unused: the proof is bound to the
    /// calling game's committed claim via `msg.sender`, which keccak-commits to
    /// exactly that `rootId`.
    function verify(bytes32, bytes calldata proof) external view returns (bool) {
        (bytes memory publicValues, bytes memory proofBytes) = abi.decode(proof, (bytes, bytes));

        // 1. Cryptographically verify the aggregation proof (reverts on failure).
        sp1Verifier.verifyProof(aggregationVKey, publicValues, proofBytes);

        AggregationOutputs memory outputs = abi.decode(publicValues, (AggregationOutputs));

        // 2. Pin the proof to this chain and proving programs.
        if (outputs.rollupConfigHash != rollupConfigHash) {
            revert RollupConfigHashMismatch(rollupConfigHash, outputs.rollupConfigHash);
        }
        if (outputs.multiBlockVKey != rangeVKeyCommitment) {
            revert RangeVKeyMismatch(rangeVKeyCommitment, outputs.multiBlockVKey);
        }

        // 3. Bind the proven transition to the challenged game's committed proposal.
        IWorldChainProofSystemGameView game = IWorldChainProofSystemGameView(msg.sender);
        bytes32 claim = game.rootClaim();
        if (outputs.l2PostRoot != claim) revert RootClaimMismatch(claim, outputs.l2PostRoot);
        uint256 blockNumber = game.l2BlockNumber();
        if (outputs.l2BlockNumber != blockNumber) revert BlockNumberMismatch(blockNumber, outputs.l2BlockNumber);
        bytes32 l1Origin = game.l1OriginHash();
        if (outputs.l1Head != l1Origin) revert L1HeadMismatch(l1Origin, outputs.l1Head);

        return true;
    }
}
