// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import "@account-abstraction/contracts/interfaces/PackedUserOperation.sol";
import {IPBHEntryPoint} from "./interfaces/IPBHEntryPoint.sol";
import {IAggregator} from "@account-abstraction/contracts/interfaces/IAggregator.sol";
import {ISafe} from "@4337/interfaces/Safe.sol";

/// @title PBH Signature Aggregator
/// @author Worldcoin
/// @dev This contract does not implement signature verification.
///         It is instead used as an identifier for Priority User Operations on World Chain.
///         Smart Accounts that return the `PBHSignatureAggregator` as the authorizer in `validationData`
///         will be considered as Priority User Operations, and will need to pack a World ID proof in the signature field.
contract PBHSignatureAggregator is IAggregator {
    /// @notice The length of an ECDSA signature.
    uint256 internal constant ECDSA_SIGNATURE_LENGTH = 65;
    /// @notice The length of the timestamp bytes.
    /// @dev 6 bytes each for validAfter and validUntil.
    uint256 internal constant TIMESTAMP_BYTES = 12;
    /// @notice The length of the encoded proof data.
    uint256 internal constant PROOF_DATA_LENGTH = 352;

    /// @notice Thrown when the Hash of the UserOperations is not
    ///         in transient storage of the `PBHVerifier`.
    error InvalidUserOperations();

    /// @notice Thrown when the length of the signature is invalid.
    error InvalidSignatureLength(uint256 expected, uint256 actual);

    /// @notice Thrown when a zero address is passed as the PBHEntryPoint.
    error ZeroAddress();

    /// @notice The PBHVerifier contract.
    IPBHEntryPoint public immutable pbhEntryPoint;

    constructor(address _pbhEntryPoint) {
        require(_pbhEntryPoint != address(0), ZeroAddress());
        pbhEntryPoint = IPBHEntryPoint(_pbhEntryPoint);
    }

    /**
     * Validate aggregated signature.
     * Revert if the aggregated signature does not match the given list of operations.
     * @param userOps   - Array of UserOperations to validate the signature for.
     */
    function validateSignatures(PackedUserOperation[] calldata userOps, bytes calldata) external view {
        bytes memory encoded = abi.encode(userOps);
        try pbhEntryPoint.validateSignaturesCallback(keccak256(encoded)) {}
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
        view
        returns (bytes memory aggregatedSignature)
    {
        IPBHEntryPoint.PBHPayload[] memory pbhPayloads = new IPBHEntryPoint.PBHPayload[](userOps.length);
        for (uint256 i = 0; i < userOps.length; ++i) {
            uint256 expectedLength =
                TIMESTAMP_BYTES + (ISafe(payable(userOps[i].sender)).getThreshold() * ECDSA_SIGNATURE_LENGTH);
            require(
                userOps[i].signature.length == expectedLength + PROOF_DATA_LENGTH,
                InvalidSignatureLength(expectedLength + PROOF_DATA_LENGTH, userOps[i].signature.length)
            );
            bytes memory proofData = userOps[i].signature[expectedLength:expectedLength + PROOF_DATA_LENGTH];
            pbhPayloads[i] = abi.decode(proofData, (IPBHEntryPoint.PBHPayload));
        }
        aggregatedSignature = abi.encode(pbhPayloads);
    }
}
