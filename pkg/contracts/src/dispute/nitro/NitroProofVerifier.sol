// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "../interfaces/IWorldChainProofVerifier.sol";
import {LibProof} from "../lib/LibProof.sol";
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
    /// @dev `proof` is the enclave's 65-byte ECDSA signature over
    ///      `keccak256(abi.encode(transition))`, matching `transition_commitment` in
    ///      `proofs/nitro/src/protocol.rs`. `tryRecover` rejects malformed lengths, high-s
    ///      values, and invalid recovery ids without reverting.
    function verify(bytes32, LibProof.TransitionPublicValues calldata transition, bytes calldata proof)
        external
        view
        returns (bool)
    {
        (address recovered, ECDSA.RecoverError err,) = ECDSA.tryRecover(keccak256(abi.encode(transition)), proof);
        return err == ECDSA.RecoverError.NoError && registry.isSignerRegistered(recovered);
    }
}
