// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "../interfaces/IWorldChainProofVerifier.sol";
import {ISP1Verifier} from "@sp1-contracts/src/ISP1Verifier.sol";

/// @title SP1ValidityVerifier
/// @author World Contributors
/// @custom:security-contact security@toolsforhumanity.com
contract SP1ValidityVerifier is IWorldChainProofVerifier {
    /// @notice Thrown when the SP1 verifier gateway address is zero.
    error ZeroSP1Verifier();

    /// @notice Succinct SP1 verifier gateway or verifier implementation.
    ISP1Verifier public immutable sp1Verifier;

    constructor(ISP1Verifier sp1Verifier_) {
        if (address(sp1Verifier_) == address(0)) revert ZeroSP1Verifier();
        sp1Verifier = sp1Verifier_;
    }

    /// @inheritdoc IWorldChainProofVerifier
    /// @dev `proof` is the SP1 on-chain proof payload; for gateway deployments its first four
    ///      bytes select the concrete verifier route. Invalid or malformed proofs revert inside
    ///      the gateway and surface as `false`.
    function verify(bytes calldata proof, bytes32 verifierId, bytes calldata publicValues)
        external
        view
        returns (bool)
    {
        try sp1Verifier.verifyProof(verifierId, publicValues, proof) {
            return true;
        } catch {
            return false;
        }
    }
}
