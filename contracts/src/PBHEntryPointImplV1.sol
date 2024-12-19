// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {PBHVerifier} from "./PBHVerifier.sol";
import {IWorldIDGroups} from "@world-id-contracts/interfaces/IWorldIDGroups.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";
import {IPBHEntryPoint} from "./interfaces/IPBHEntryPoint.sol";

/// @title PBH Entry Point Implementation V1
/// @dev This contract is an implementation of the PBH Entry Point.
///      It is used to verify the signature of a Priority User Operation, and Relaying Priority Bundles to the EIP-4337 Entry Point.
/// @author Worldcoin
contract PBHEntryPointImplV1 is IPBHEntryPoint, PBHVerifier {
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

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  Events                                ///
    //////////////////////////////////////////////////////////////////////////////

    event PBHEntryPointImplInitialized(
        IWorldIDGroups indexed worldId, IEntryPoint indexed entryPoint, uint8 indexed numPbhPerMonth
    );

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  Vars                                  ///
    //////////////////////////////////////////////////////////////////////////////

    /// @notice Transient Storage for the hashed operations.
    /// @dev The PBHSignatureAggregator will cross reference this slot to ensure
    ///     The PBHVerifier is always the proxy to the EntryPoint for PBH Bundles.
    bytes32 internal _hashedOps;

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
    /// @param worldId The World ID instance that will be used for verifying proofs. If set to the
    ///        0 addess, then it will be assumed that verification will take place off chain.
    /// @param entryPoint The ERC-4337 Entry Point.
    /// @param _numPbhPerMonth The number of allowed PBH transactions per month.
    ///
    /// @custom:reverts string If called more than once at the same initialisation number.
    function initialize(IWorldIDGroups worldId, IEntryPoint entryPoint, uint8 _numPbhPerMonth)
        external
        reinitializer(1)
    {
        // First, ensure that all of the parent contracts are initialised.
        __delegateInit();

        _worldId = worldId;
        _entryPoint = entryPoint;
        numPbhPerMonth = _numPbhPerMonth;

        // Say that the contract is initialized.
        __setInitialized();
        emit PBHEntryPointImplInitialized(worldId, entryPoint, _numPbhPerMonth);
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

    /// Execute a batch of UserOperation with Aggregators
    /// @param opsPerAggregator - The operations to execute, grouped by aggregator (or address(0) for no-aggregator accounts).
    /// @param beneficiary      - The address to receive the fees.
    function handleAggregatedOps(
        IEntryPoint.UserOpsPerAggregator[] calldata opsPerAggregator,
        address payable beneficiary
    ) external virtual onlyProxy onlyInitialized {
        bytes32 hashedOps = keccak256(abi.encode(opsPerAggregator));
        assembly ("memory-safe") {
            tstore(_hashedOps.slot, hashedOps)
        }

        for (uint256 i = 0; i < opsPerAggregator.length; ++i) {
            PBHPayload[] memory pbhPayloads = abi.decode(opsPerAggregator[i].signature, (PBHPayload[]));
            for (uint256 j = 0; j < pbhPayloads.length; ++j) {
                verifyPbhProof(
                    opsPerAggregator[i].userOps[j].sender,
                    opsPerAggregator[i].userOps[j].nonce,
                    opsPerAggregator[i].userOps[j].callData,
                    pbhPayloads[j]
                );
            }
        }

        _entryPoint.handleAggregatedOps(opsPerAggregator, beneficiary);
    }

    /// @notice Validates the hashed operations is the same as the hash transiently stored.
    /// @param hashedOps The hashed operations to validate.
    function validateSignaturesCallback(bytes32 hashedOps) external view virtual onlyProxy onlyInitialized {
        assembly ("memory-safe") {
            if iszero(eq(tload(_hashedOps.slot), hashedOps)) { revert(0, 0) }
        }
    }

    // TODO: PBH Multicall Entry Point
}
