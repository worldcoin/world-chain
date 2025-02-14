// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import "@account-abstraction/contracts/interfaces/PackedUserOperation.sol";
import {IAggregator} from "@account-abstraction/contracts/interfaces/IAggregator.sol";
import {ISafe} from "@4337/interfaces/Safe.sol";
import {IWorldID} from "@world-id-contracts/interfaces/IWorldID.sol";
import {IPBHEntryPoint} from "./interfaces/IPBHEntryPoint.sol";
import {ByteHasher} from "./lib/ByteHasher.sol";
import {SafeModuleSignatures} from "./lib/SafeModuleSignatures.sol";

/// @title PBH Signature Aggregator
/// @author Worldcoin
/// @dev This contract does not implement signature verification.
///         It is instead used as an identifier for Priority User Operations on World Chain.
///         Smart Accounts that return the `PBHSignatureAggregator` as the authorizer in `validationData`
///         will be considered as Priority User Operations, and will need to pack a World ID proof in the signature field.
/// @custom:security-contact security@toolsforhumanity.com
contract PBHSignatureAggregator is IAggregator {
    using ByteHasher for bytes;

    ///////////////////////////////////////////////////////////////////////////////
    ///                             STATE VARIABLES                             ///
    //////////////////////////////////////////////////////////////////////////////

    /// @notice The PBHVerifier contract.
    IPBHEntryPoint public immutable pbhEntryPoint;

    /// @notice The WorldID contract.
    IWorldID public immutable worldID;

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  ERRORS                                ///
    //////////////////////////////////////////////////////////////////////////////

    /// @notice Thrown when a zero address is passed as the PBHEntryPoint.
    error AddressZero();

    ///////////////////////////////////////////////////////////////////////////////
    ///                               FUNCTIONS                                 ///
    ///////////////////////////////////////////////////////////////////////////////

    constructor(address _pbhEntryPoint, address _worldID) {
        require(_pbhEntryPoint != address(0), AddressZero());
        require(_worldID != address(0), AddressZero());
        pbhEntryPoint = IPBHEntryPoint(_pbhEntryPoint);
        worldID = IWorldID(_worldID);
    }

    /**
     * Validate aggregated signature.
     * Revert if the aggregated signature does not match the given list of operations.
     * @param userOps   - Array of UserOperations to validate the signature for.
     */
    function validateSignatures(PackedUserOperation[] calldata userOps, bytes calldata) external view {
        bytes memory encoded = abi.encode(userOps);
        pbhEntryPoint.validateSignaturesCallback(keccak256(encoded));
    }

    /**
     * Validate signature of a single userOp.
     * This method should be called off chain by the bundler to verify the integrity of the encoded signature as
     * well as verify the proof data. The proof data will then be stripped off the signature, and the remaining
     * `sigForUserOp` should be passed to handleAggregatedOps.
     * @param userOp        - The userOperation received from the user.
     * @return sigForUserOp - The new userOperation signature.
     */
    function validateUserOpSignature(PackedUserOperation calldata userOp)
        external
        view
        returns (bytes memory sigForUserOp)
    {
        bytes memory proofData;

        (sigForUserOp, proofData) =
            SafeModuleSignatures.extractProof(userOp.signature, ISafe(payable(userOp.sender)).getThreshold());
        IPBHEntryPoint.PBHPayload memory pbhPayload = abi.decode(proofData, (IPBHEntryPoint.PBHPayload));

        // We now generate the signal hash from the sender, nonce, and calldata
        uint256 signalHash = abi.encodePacked(userOp.sender, userOp.nonce, userOp.callData).hashToField();

        pbhEntryPoint.verifyPbh(signalHash, pbhPayload);

        // If the worldID is not set, we need to verify the semaphore proof
        if (address(pbhEntryPoint.worldId()) == address(0)) {
            worldID.verifyProof(
                pbhPayload.root, signalHash, pbhPayload.nullifierHash, pbhPayload.pbhExternalNullifier, pbhPayload.proof
            );
        }
    }

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
            (, bytes memory proofData) = SafeModuleSignatures.extractProof(
                userOps[i].signature, ISafe(payable(userOps[i].sender)).getThreshold()
            );
            pbhPayloads[i] = abi.decode(proofData, (IPBHEntryPoint.PBHPayload));
        }
        aggregatedSignature = abi.encode(pbhPayloads);
    }
}
