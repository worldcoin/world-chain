// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IWorldChainProofVerifier} from "./IWorldChainProofVerifier.sol";

/// @notice Minimal SP1-style verifier gateway shape.
/// @dev Real gateway verifies a proof against a program image id and public
///      journal. The journal MUST commit to `rootId`.
interface ISP1Verifier {
    /// @param imageId The program verification key / image id.
    /// @param journal The committed public values.
    /// @param proof The encoded proof bytes.
    function verifyProof(bytes32 imageId, bytes calldata journal, bytes calldata proof) external view;
}

/// @title ZKVerifier
/// @notice WIP-1006 `VALIDITY_PROOF` lane verifier. Wraps an SP1-style gateway
///         and enforces that the proof journal binds to `rootId`.
/// @dev The lane verifier constant `imageId` (the validity-proof program key)
///      lives here and is committed to in the proof journal, per WIP-1006.
contract ZKVerifier is IWorldChainProofVerifier {
    /// @notice The verification gateway.
    ISP1Verifier public immutable gateway;

    /// @notice The validity-proof program image id committed in the journal.
    bytes32 public immutable imageId;

    /// @notice Thrown when the journal does not bind to the expected `rootId`.
    error JournalRootIdMismatch(bytes32 expected, bytes32 actual);

    /// @param gateway_ The SP1-style verification gateway.
    /// @param imageId_ The validity-proof program image id.
    constructor(ISP1Verifier gateway_, bytes32 imageId_) {
        gateway = gateway_;
        imageId = imageId_;
    }

    /// @inheritdoc IWorldChainProofVerifier
    /// @dev `proof` is `abi.encode(bytes journal, bytes zkProof)`. The first
    ///      32 bytes of the journal MUST equal `rootId`.
    function verify(bytes32 rootId, bytes calldata proof) external view returns (bool) {
        (bytes memory journal, bytes memory zkProof) = abi.decode(proof, (bytes, bytes));

        if (journal.length < 32) revert JournalRootIdMismatch(rootId, bytes32(0));
        bytes32 journalRootId;
        assembly {
            journalRootId := mload(add(journal, 0x20))
        }
        if (journalRootId != rootId) revert JournalRootIdMismatch(rootId, journalRootId);

        // Reverts on invalid proof; returns true only if the gateway accepts.
        gateway.verifyProof(imageId, journal, zkProof);
        return true;
    }
}
