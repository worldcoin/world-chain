// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {ByteHasher} from "./helpers/ByteHasher.sol";
import {PBHExternalNullifier} from "./helpers/PBHExternalNullifier.sol";
import {IWorldIDGroups} from "@world-id-contracts/interfaces/IWorldIDGroups.sol";
import {WorldIDImpl} from "@world-id-contracts/abstract/WorldIDImpl.sol";
import "@BokkyPooBahsDateTimeLibrary/BokkyPooBahsDateTimeLibrary.sol";

/// @title PBH Verifier Implementation Version 1
/// @author Worldcoin
/// @notice An implementation of a priority blockspace for humans (PBH) transaction verifier.
/// @dev This is the implementation delegated to by a proxy.
contract PBHVerifierImplV1 is WorldIDImpl {
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

    event PBHVerifierImplInitialized(IWorldIDGroups indexed worldId, uint8 indexed numPbhPerMonth);

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  Vars                                  ///
    //////////////////////////////////////////////////////////////////////////////

    /// @dev The World ID group ID (always 1)
    uint256 public immutable GROUP_ID = 1;

    /// @dev The World ID instance that will be used for verifying proofs
    IWorldIDGroups public worldId;

    /// @dev Make this configurable
    uint8 public numPbhPerMonth;

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  Mappings                              ///
    //////////////////////////////////////////////////////////////////////////////

    /// @dev Whether a nullifier hash has been used already. Used to guarantee an action is only performed once by a single person
    mapping(uint256 => bool) public nullifierHashes;

    ///////////////////////////////////////////////////////////////////////////////
    ///                             INITIALIZATION                              ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Constructs the contract.
    constructor() {
        // When called in the constructor, this is called in the context of the implementation and
        // not the proxy. Calling this thereby ensures that the contract cannot be spuriously
        // initialized on its own.
        _disableInitializers();
    }

    /// @notice Initializes the contract.
    /// @dev Must be called exactly once.
    /// @dev This is marked `reinitializer()` to allow for updated initialisation steps when working
    ///      with upgrades based upon this contract. Be aware that there are only 256 (zero-indexed)
    ///      initialisations allowed, so decide carefully when to use them. Many cases can safely be
    ///      replaced by use of setters.
    /// @dev This function is explicitly not virtual as it does not make sense to override even when
    ///      upgrading. Create a separate initializer function instead.
    ///
    /// @param _worldId The World ID instance that will be used for verifying proofs. If set to the
    ///        0 addess, then it will be assumed that verification will take place off chain.
    /// @param _numPbhPerMonth The number of allowed PBH transactions per month.
    ///
    /// @custom:reverts string If called more than once at the same initialisation number.
    function initialize(IWorldIDGroups _worldId, uint8 _numPbhPerMonth) public reinitializer(1) {
        // First, ensure that all of the parent contracts are initialised.
        __delegateInit();

        worldId = _worldId;
        numPbhPerMonth = _numPbhPerMonth;

        // Say that the contract is initialized.
        __setInitialized();

        emit PBHVerifierImplInitialized(_worldId, _numPbhPerMonth);
    }

    /// @notice Responsible for initialising all of the supertypes of this contract.
    /// @dev Must be called exactly once.
    /// @dev When adding new superclasses, ensure that any initialization that they need to perform
    ///      is accounted for here.
    ///
    /// @custom:reverts string If called more than once.
    function __delegateInit() internal virtual onlyInitializing {
        __WorldIDImpl_init();
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  Functions                             ///
    //////////////////////////////////////////////////////////////////////////////

    /// @notice Verifies a PBH proof.
    /// @param root The root of the Merkle tree that this proof is valid for.
    /// @param sender The sender of this particular transaction or UserOp.
    /// @param nonce Transaction/UserOp nonce.
    /// @param callData Transaction/UserOp call data.
    /// @param pbhExternalNullifier External nullifier encodeing month, year, and a pbhNonce.
    /// @param nullifierHash Nullifier hash for this semaphore proof.
    /// @param proof The zero-knowledge proof that demonstrates the claimer is registered with World ID.
    function verifyPbhProof(
        uint256 root,
        address sender,
        uint256 nonce,
        bytes memory callData,
        uint256 pbhExternalNullifier,
        uint256 nullifierHash,
        uint256[8] memory proof
    ) external virtual onlyProxy onlyInitialized {
        // First, we make sure this nullifier has not been used before.
        if (nullifierHashes[nullifierHash]) revert InvalidNullifier();

        // Verify the external nullifier
        PBHExternalNullifier.verify(pbhExternalNullifier, numPbhPerMonth);

        // If worldId address is set, proceed with on chain verification,
        // otherwise assume verification has been done off chain by the builder.
        if (address(worldId) != address(0)) {
            // We now generate the signal hash from the sender, nonce, and calldata
            uint256 signalHash = abi.encodePacked(sender, nonce, callData).hashToField();

            // We now verify the provided proof is valid and the user is verified by World ID
            worldId.verifyProof(root, GROUP_ID, signalHash, nullifierHash, pbhExternalNullifier, proof);
        }

        // We now record the user has done this, so they can't do it again (proof of uniqueness)
        nullifierHashes[nullifierHash] = true;

        emit PBH(root, sender, nonce, callData, pbhExternalNullifier, nullifierHash, proof);
    }

    /// @notice Sets a new value for numPbhPerMonth.
    /// @param _newValue The new value set.
    /// @dev Can only be set by the owner.
    function setNumPbhPerMonth(uint8 _newValue) external virtual onlyOwner onlyProxy onlyInitialized {
        numPbhPerMonth = _newValue;
    }
}
