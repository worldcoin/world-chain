// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "../interfaces/IWorldChainProofVerifier.sol";
import {LibProof, TransitionPublicValues} from "../lib/LibProof.sol";
import {NitroEnclaveKeyRegistry} from "./NitroEnclaveKeyRegistry.sol";
import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";

/// @title NitroProofVerifier
/// @author World Contributors
/// @custom:security-contact security@toolsforhumanity.com
contract NitroProofVerifier is IWorldChainProofVerifier {
    /// @notice Registry of attested enclave keys.
    NitroEnclaveKeyRegistry public immutable registry;

    /// @param registry_ The Nitro enclave key registry to consult.
    constructor(NitroEnclaveKeyRegistry registry_) {
        registry = registry_;
    }

    /// @inheritdoc IWorldChainProofVerifier
    function verify(bytes32, TransitionPublicValues calldata transition, bytes calldata proof)
        external
        view
        returns (bool)
    {
        (address recovered, ECDSA.RecoverError err,) = ECDSA.tryRecover(keccak256(abi.encode(transition)), proof);
        return err == ECDSA.RecoverError.NoError && registry.isSignerRegistered(recovered);
    }
}
