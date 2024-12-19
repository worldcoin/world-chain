// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {ByteHasher} from "./helpers/ByteHasher.sol";
import {PBHExternalNullifier} from "./helpers/PBHExternalNullifier.sol";
import {IWorldIDGroups} from "@world-id-contracts/interfaces/IWorldIDGroups.sol";
import {WorldIDImpl} from "@world-id-contracts/abstract/WorldIDImpl.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";
import {IPBHVerifier} from "./interfaces/IPBHVerifier.sol";
import "@BokkyPooBahsDateTimeLibrary/BokkyPooBahsDateTimeLibrary.sol";

/// @title PBH Verifier Implementation Version 1
/// @author Worldcoin
/// @notice An implementation of a priority blockspace for humans (PBH) transaction verifier.
/// @dev This is the implementation delegated to by a proxy.
contract PBHVerifier is IPBHVerifier, WorldIDImpl {
    ///////////////////////////////////////////////////////////////////////////////
    ///                   A NOTE ON IMPLEMENTATION CONTRACTS                    ///
    ///////////////////////////////////////////////////////////////////////////////

    // This contract is designed explicitly to operate from behind a proxy contract. As a result,
    // there are a few important implementation considerations:
    //
    // - All updates made after deploying a given version of the implementation should inherit from
    //   the latest version of the implementation. This prevents storage clashes.
    // - All functions that are less access-restricted than `private` should be marked `virtual` in
    //   order to enable the fixing of bugs in the existing interface.
    // - Any function that reads from or modifies state (i.e. is not marked `pure`) must be
    //   annotated with the `onlyProxy` and `onlyInitialized` modifiers. This ensures that it can
    //   only be called when it has access to the data in the proxy, otherwise results are likely to
    //   be nonsensical.
    // - This contract deals with important data for the PBH system. Ensure that all newly-added
    //   functionality is carefully access controlled using `onlyOwner`, or a more granular access
    //   mechanism.
    // - Do not assign any contract-level variables at the definition site unless they are
    //   `constant`.
    //
    // Additionally, the following notes apply:
    //
    // - Initialisation and ownership management are not protected behind `onlyProxy` intentionally.
    //   This ensures that the contract can safely be disposed of after it is no longer used.
    // - Carefully consider what data recovery options are presented as new functionality is added.
    //   Care must be taken to ensure that a migration plan can exist for cases where upgrades
    //   cannot recover from an issue or vulnerability.

    ///////////////////////////////////////////////////////////////////////////////
    ///                    !!!!! DATA: DO NOT REORDER !!!!!                     ///
    ///////////////////////////////////////////////////////////////////////////////

    // To ensure compatibility between upgrades, it is exceedingly important that no reordering of
    // these variables takes place. If reordering happens, a storage clash will occur (effectively a
    // memory safety error).

    using ByteHasher for bytes;

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  ERRORS                                ///
    //////////////////////////////////////////////////////////////////////////////

    /// @notice Thrown when attempting to reuse a nullifier
    error InvalidNullifier();

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  Events                                ///
    //////////////////////////////////////////////////////////////////////////////

    /// @notice Emitted once for each successful PBH verification.
    ///
    /// @param root The root of the Merkle tree that this proof is valid for.
    /// @param sender The sender of this particular transaction or UserOp.
    /// @param nonce Transaction/UserOp nonce.
    /// @param callData Transaction/UserOp call data.
    /// @param pbhExternalNullifier External nullifier encoding month, year, and a pbhNonce.
    /// @param nullifierHash Nullifier hash for this semaphore proof.
    /// @param proof The zero-knowledge proof that demonstrates the claimer is registered with World ID.
    event PBH(
        uint256 indexed root,
        address indexed sender,
        uint256 nonce,
        bytes callData,
        uint256 indexed pbhExternalNullifier,
        uint256 nullifierHash,
        uint256[8] proof
    );

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  Vars                                  ///
    //////////////////////////////////////////////////////////////////////////////

    /// @dev Gap for transient storage.
    uint256 private __gap;

    /// @dev The World ID group ID (always 1)
    uint256 internal immutable _GROUP_ID = 1;

    /// @dev The World ID instance that will be used for verifying proofs
    IWorldIDGroups internal _worldId;

    /// @dev The EntryPoint where Aggregated PBH Bundles will be proxied to.
    IEntryPoint internal _entryPoint;

    /// @notice The number of PBH transactions that may be used by a single
    ///         World ID in a given month.
    uint8 public numPbhPerMonth;

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  Mappings                              ///
    //////////////////////////////////////////////////////////////////////////////

    /// @dev Whether a nullifier hash has been used already. Used to guarantee an action is only performed once by a single person
    mapping(uint256 => bool) public nullifierHashes;

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  Functions                             ///
    //////////////////////////////////////////////////////////////////////////////

    /// @param sender The sender of this particular transaction or UserOp.
    /// @param nonce Transaction/UserOp nonce.
    /// @param callData Transaction/UserOp call data.
    /// @param pbhPayload The PBH payload containing the proof data.
    function verifyPbhProof(address sender, uint256 nonce, bytes memory callData, PBHPayload memory pbhPayload)
        public
        virtual
        onlyInitialized
        onlyProxy
    {
        // First, we make sure this nullifier has not been used before.
        if (nullifierHashes[pbhPayload.nullifierHash]) revert InvalidNullifier();

        // Verify the external nullifier
        PBHExternalNullifier.verify(pbhPayload.pbhExternalNullifier, numPbhPerMonth);

        // If worldId address is set, proceed with on chain verification,
        // otherwise assume verification has been done off chain by the builder.
        if (address(_worldId) != address(0)) {
            // We now generate the signal hash from the sender, nonce, and calldata
            uint256 signalHash = abi.encodePacked(sender, nonce, callData).hashToField();

            // We now verify the provided proof is valid and the user is verified by World ID
            _worldId.verifyProof(
                pbhPayload.root,
                _GROUP_ID,
                signalHash,
                pbhPayload.nullifierHash,
                pbhPayload.pbhExternalNullifier,
                pbhPayload.proof
            );
        }

        // We now record the user has done this, so they can't do it again (proof of uniqueness)
        nullifierHashes[pbhPayload.nullifierHash] = true;

        emit PBH(
            pbhPayload.root,
            sender,
            nonce,
            callData,
            pbhPayload.pbhExternalNullifier,
            pbhPayload.nullifierHash,
            pbhPayload.proof
        );
    }

    /// @notice Sets the number of PBH transactions allowed per month.
    /// @param _numPbhPerMonth The number of allowed PBH transactions per month.
    function setNumPbhPerMonth(uint8 _numPbhPerMonth) external virtual onlyOwner onlyProxy onlyInitialized {
        numPbhPerMonth = _numPbhPerMonth;
    }

    /// @dev If the World ID address is set to 0, then it is assumed that verification will take place off chain.
    /// @notice Sets the World ID instance that will be used for verifying proofs.
    /// @param __worldId The World ID instance that will be used for verifying proofs.
    function setWorldId(address __worldId) external virtual onlyOwner onlyProxy onlyInitialized {
        _worldId = IWorldIDGroups(__worldId);
    }
}
