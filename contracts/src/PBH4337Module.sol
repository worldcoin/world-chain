// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Safe4337Module} from "@4337/Safe4337Module.sol";
import {PackedUserOperation} from "@account-abstraction/contracts/interfaces/PackedUserOperation.sol";
import {ValidationData} from "@account-abstraction/contracts/core/Helpers.sol";
import {_packValidationData} from "@account-abstraction/contracts/core/Helpers.sol";
import {ISafe} from "@4337/interfaces/Safe.sol";

contract PBHSafe4337Module is Safe4337Module {
    /// @notice The length of an ECDSA signature.
    uint256 internal constant ECDSA_SIGNATURE_LENGTH = 65;

    /// @notice The length of the timestamp bytes.
    /// @dev 6 bytes each for validAfter and validUntil.
    uint256 internal constant TIMESTAMP_BYTES = 12; 

    /// @notice The length of the encoded proof data.
    uint256 internal constant ENCODED_PROOF_BYTES = 352;

    /// @notice The PBH Signature Aggregator address.
    address public immutable PBH_SIGNATURE_AGGREGATOR;

    /// @notice The PBH Nonce Key.
    /// @dev This key is used to identify a PBH user operation.
    uint192 public immutable PBH_NONCE_KEY;

    /// @notice Thrown when the proof size is invalid.
    error InvalidProofSize();

    /// @notice Thrown when a null address is passed in the constructor.
    error ZeroAddress();

    constructor(address entryPoint, address _pbhSignatureAggregator, uint192 _pbhNonceKey) Safe4337Module(entryPoint) {
        require(_pbhSignatureAggregator != address(0), ZeroAddress());  
        require(entryPoint != address(0), ZeroAddress());
        PBH_SIGNATURE_AGGREGATOR = _pbhSignatureAggregator;
        PBH_NONCE_KEY = _pbhNonceKey;
    }

    // TODO: Fork the Safe4337Module dependency and add 'override' to _validateSignatures. It is manually updated currently and CI will fail
    /**
     * @dev Validates that the user operation is correctly signed and returns an ERC-4337 packed validation data
     * of `validAfter || validUntil || authorizer`:
     *  - `authorizer`: 20-byte address, 0 for valid signature or 1 to mark signature failure (this module does not make use of signature aggregators).
     *  - `validUntil`: 6-byte timestamp value, or zero for "infinite". The user operation is valid only up to this time.
     *  - `validAfter`: 6-byte timestamp. The user operation is valid only after this time.
     * @param userOp User operation struct.
     * @return validationData An integer indicating the result of the validation.
     */
    function _validateSignatures(PackedUserOperation calldata userOp)
        internal
        view
        override
        returns (uint256 validationData)
    {
        // Check if the userOp has the specified PBH key
        // https://github.com/eth-infinitism/account-abstraction/blob/develop/contracts/core/NonceManager.sol#L38
        uint192 key = uint192(userOp.nonce >> 64);

        // This does NOT validate the proof
        // It removes the first 12 bytes from the signature as it represents the validAfter and validUntil values
        // operationData is not determined by the signature
        (bytes memory operationData, uint48 validAfter, uint48 validUntil, bytes calldata signatures) =
            _getSafeOp(userOp);

        // If it is a PBH transaction, we need to handle two cases with the signature:
        // 1. The bundler simulates the call with the proof appended
        // 2. UserOp execution without proof appended
        bool isPBH = (key == PBH_NONCE_KEY);

        // Base signature length calculation:
        // TIMESTAMP_BYTES (12) + (threshold * ECDSA_SIGNATURE_LENGTH)
        uint256 expectedLength =
            TIMESTAMP_BYTES + (ISafe(payable(userOp.sender)).getThreshold() * ECDSA_SIGNATURE_LENGTH);

        // If the signature length is greater than the expected length, then we know that the bundler appended the proof
        // We need to remove the proof from the signature before validation
        if (isPBH && userOp.signature.length > expectedLength) {
            if (userOp.signature.length - expectedLength != ENCODED_PROOF_BYTES) {
                revert InvalidProofSize();
            }

            // Remove the proof from the signature
            signatures = userOp.signature[TIMESTAMP_BYTES:expectedLength];
        }

        // The `checkSignatures` function in the Safe contract does not force a fixed size on signature length.
        // A malicious bundler can pad the Safe operation `signatures` with additional bytes, causing the account to pay
        // more gas than needed for user operation validation (capped by `verificationGasLimit`).
        // `_checkSignaturesLength` ensures that there are no additional bytes in the `signature` than are required.
        bool validSignature = _checkSignaturesLength(signatures, ISafe(payable(userOp.sender)).getThreshold());

        try ISafe(payable(userOp.sender)).checkSignatures(keccak256(operationData), operationData, signatures) {}
        catch {
            validSignature = false;
        }

        address authorizer;

        // If the signature is valid and the userOp is a PBH userOp, return the PBH signature aggregator as the authorizer
        // Else return 0 for valid signature and 1 for invalid signature
        if (isPBH && validSignature) {
            authorizer = PBH_SIGNATURE_AGGREGATOR;
        } else {
            authorizer = validSignature ? address(0) : address(1);
        }

        // The timestamps are validated by the entry point, therefore we will not check them again.
        validationData = _packValidationData(ValidationData(authorizer, validUntil, validAfter));
    }
}
