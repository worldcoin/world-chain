// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "@account-abstraction/contracts/interfaces/PackedUserOperation.sol";
import {IPBHEntryPoint} from "./interfaces/IPBHEntryPoint.sol";
import {IPBHVerifier} from "./interfaces/IPBHVerifier.sol";
import {IAggregator} from "@account-abstraction/contracts/interfaces/IAggregator.sol";

/// @title PBH Signature Aggregator
/// @author Worldcoin
/// @notice This contract does not implement signature verification.
///         It is instead used as an identifier for Priority User Operations on World Chain.
///         Smart Accounts that return the `PBHSignatureAggregator` as the authorizer in `validationData`
///         will be considered as Priority User Operations, and will need to pack a World ID proof in the signature field.
contract PBHSignatureAggregator is IAggregator {
    /// @notice Thrown when the Hash of the UserOperations is not
    ///         in transient storage of the `PBHVerifier`.
    error InvalidUserOperations();

    /// @notice The PBHVerifier contract.
    IPBHEntryPoint internal immutable _pbhEntryPoint;

    constructor(address __pbhEntryPoint) {
        _pbhEntryPoint = IPBHEntryPoint(__pbhEntryPoint);
    }

    /**
     * Validate aggregated signature.
     * Revert if the aggregated signature does not match the given list of operations.
     * @param userOps   - Array of UserOperations to validate the signature for.
     */
    function validateSignatures(PackedUserOperation[] calldata userOps, bytes calldata) external view {
        bytes memory encoded = abi.encode(userOps);
        try _pbhEntryPoint.validateSignaturesCallback(keccak256(encoded)) {}
        catch {
            revert InvalidUserOperations();
        }
    }

    /**
     * Validate signature of a single userOp.
     * This method should be called by bundler after EntryPointSimulation.simulateValidation() returns
     * the aggregator this account uses.
     * First it validates the signature over the userOp. Then it returns data to be used when creating the handleOps.
     * @param userOp        - The userOperation received from the user.
     * @return sigForUserOp - The value to put into the signature field of the userOp when calling handleOps.
     *                        (usually empty, unless account and aggregator support some kind of "multisig".
     */
    function validateUserOpSignature(PackedUserOperation calldata userOp)
        external
        pure
        returns (bytes memory sigForUserOp)
    {}

    /**
     * Aggregate multiple signatures into a single value.
     * This method is called off-chain to calculate the signature to pass with handleOps()
     * bundler MAY use optimized custom code perform this aggregation.
     * @param userOps              - Array of UserOperations to collect the signatures from.
     * @return aggregatedSignature - The aggregated signature.
     */
    function aggregateSignatures(PackedUserOperation[] calldata userOps)
        external
        pure
        returns (bytes memory aggregatedSignature)
    {
        IPBHVerifier.PBHPayload[] memory pbhPayloads = new IPBHVerifier.PBHPayload[](userOps.length);
        for (uint256 i = 0; i < userOps.length; ++i) {
            // Bytes (0:65) - UserOp Signature
            // Bytes (65:65 + 352) - Packed Proof Data
            bytes memory proofData = userOps[i].signature[65:];
            pbhPayloads[i] = abi.decode(proofData, (IPBHVerifier.PBHPayload));
        }
        aggregatedSignature = abi.encode(pbhPayloads);
    }
}
