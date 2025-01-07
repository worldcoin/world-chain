// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IWorldID} from "@world-id-contracts/interfaces/IWorldID.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";
import {IPBHEntryPoint} from "./interfaces/IPBHEntryPoint.sol";
import {IMulticall3} from "./interfaces/IMulticall3.sol";
import {ByteHasher} from "./helpers/ByteHasher.sol";
import {PBHExternalNullifier} from "./helpers/PBHExternalNullifier.sol";
import {WorldIDImpl} from "@world-id-contracts/abstract/WorldIDImpl.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import "@BokkyPooBahsDateTimeLibrary/BokkyPooBahsDateTimeLibrary.sol";

/// @title PBH Entry Point Implementation V1
/// @dev This contract is an implementation of the PBH Entry Point.
///      It is used to verify the signature of a Priority User Operation, and Relaying Priority Bundles to the EIP-4337 Entry Point.
/// @author Worldcoin
contract PBHEntryPointImplV1 is IPBHEntryPoint, WorldIDImpl, ReentrancyGuard {
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

    /// @notice Error thrown when the address is 0
    error AddressZero();

    /// @notice Error thrown when the number of PBH transactions allowed per month is 0
    error InvalidNumPbhPerMonth();

    /// @notice Thrown when transient storage slot collides with another set slot
    error StorageCollision();

    /// @notice Thrown when the hash of the user operations is invalid
    error InvalidHashedOps();

    /// @notice Thrown when the gas limit for a PBH multicall transaction is exceeded
    error GasLimitExceeded(uint256 gasLimit);

    /// @notice Thrown when setting the gas limit for a PBH multicall to 0
    error InvalidPBHGasLimit(uint256 gasLimit);

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  Events                                ///
    //////////////////////////////////////////////////////////////////////////////

    /// @notice Emitted when the contract is initialized.
    /// @param worldId The World ID instance that will be used for verifying proofs.
    /// @param entryPoint The ERC-4337 Entry Point.
    /// @param numPbhPerMonth The number of allowed PBH transactions per month.
    /// @param multicall3 Address of the Multicall3 implementation.
    event PBHEntryPointImplInitialized(
        IWorldID indexed worldId,
        IEntryPoint indexed entryPoint,
        uint8 indexed numPbhPerMonth,
        address multicall3,
        uint256 pbhGasLimit
    );

    /// @notice Emitted once for each successful PBH verification.
    ///
    /// @param sender The sender of this particular transaction or UserOp.
    /// @param signalHash Signal hash associated with the PBHPayload.
    /// @param payload The zero-knowledge proof that demonstrates the claimer is registered with World ID.
    event PBH(address indexed sender, uint256 indexed signalHash, PBHPayload payload);

    /// @notice Emitted when the World ID address is set.
    ///
    /// @param worldId The World ID instance that will be used for verifying proofs.
    event WorldIdSet(address indexed worldId);

    /// @notice Emitted when the number of PBH transactions allowed per month is set.
    ///
    /// @param numPbhPerMonth The number of allowed PBH transactions per month.
    event NumPbhPerMonthSet(uint8 indexed numPbhPerMonth);

    /// @notice Emitted when setting the PBH gas limit.
    ///
    /// @param pbhGasLimit The gas limit for a PBH multicall transaction.
    event PBHGasLimitSet(uint256 indexed pbhGasLimit);

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  Vars                                  ///
    //////////////////////////////////////////////////////////////////////////////

    /// @dev The World ID instance that will be used for verifying proofs
    IWorldID public worldId;

    /// @dev The EntryPoint where Aggregated PBH Bundles will be proxied to.
    IEntryPoint public entryPoint;

    /// @notice The number of PBH transactions that may be used by a single
    ///         World ID in a given month.
    uint8 public numPbhPerMonth;

    /// @notice Address of the Multicall3 implementation.
    address internal multicall3;

    /// @dev Whether a nullifier hash has been used already. Used to guarantee an action is only performed once by a single person
    mapping(uint256 => bool) public nullifierHashes;

    /// @notice The gas limit for a PBH multicall transaction
    uint256 public pbhGasLimit;

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
    /// @param _entryPoint The ERC-4337 Entry Point.
    /// @param _numPbhPerMonth The number of allowed PBH transactions per month.
    ///
    /// @custom:reverts string If called more than once at the same initialisation number.
    function initialize(
        IWorldID _worldId,
        IEntryPoint _entryPoint,
        uint8 _numPbhPerMonth,
        address _multicall3,
        uint256 _pbhGasLimit
    ) external reinitializer(1) {
        if (address(_worldId) == address(0) || address(_entryPoint) == address(0) || _multicall3 == address(0)) {
            revert AddressZero();
        }

        if (_numPbhPerMonth == 0) {
            revert InvalidNumPbhPerMonth();
        }

        // First, ensure that all of the parent contracts are initialised.
        __delegateInit();

        worldId = _worldId;
        entryPoint = _entryPoint;
        numPbhPerMonth = _numPbhPerMonth;
        multicall3 = _multicall3;

        if (_pbhGasLimit == 0 || _pbhGasLimit > block.gaslimit) {
            revert InvalidPBHGasLimit(_pbhGasLimit);
        }

        pbhGasLimit = _pbhGasLimit;
        // Say that the contract is initialized.
        __setInitialized();
        emit PBHEntryPointImplInitialized(_worldId, _entryPoint, _numPbhPerMonth, _multicall3, _pbhGasLimit);
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

    /// @param pbhPayload The PBH payload containing the proof data.
    function verifyPbh(uint256 signalHash, PBHPayload memory pbhPayload)
        public
        view
        virtual
        onlyProxy
        onlyInitialized
    {
        // First, we make sure this nullifier has not been used before.
        if (nullifierHashes[pbhPayload.nullifierHash]) {
            revert InvalidNullifier();
        }

        // Verify the external nullifier
        PBHExternalNullifier.verify(pbhPayload.pbhExternalNullifier, numPbhPerMonth);

        // If worldId address is set, proceed with on chain verification,
        // otherwise assume verification has been done off chain by the builder.
        if (address(worldId) != address(0)) {
            // We now verify the provided proof is valid and the user is verified by World ID
            worldId.verifyProof(
                pbhPayload.root, signalHash, pbhPayload.nullifierHash, pbhPayload.pbhExternalNullifier, pbhPayload.proof
            );
        }
    }

    /// Execute a batch of PackedUserOperation with Aggregators
    /// @param opsPerAggregator - The operations to execute, grouped by aggregator (or address(0) for no-aggregator accounts).
    /// @param beneficiary      - The address to receive the fees.
    function handleAggregatedOps(
        IEntryPoint.UserOpsPerAggregator[] calldata opsPerAggregator,
        address payable beneficiary
    ) external virtual onlyProxy onlyInitialized nonReentrant {
        for (uint256 i = 0; i < opsPerAggregator.length; ++i) {
            bytes32 hashedOps = keccak256(abi.encode(opsPerAggregator[i].userOps));
            assembly ("memory-safe") {
                if gt(tload(hashedOps), 0) {
                    mstore(0x00, 0x5e75ad06) // StorageCollision()
                    revert(0x1c, 0x04)
                }

                tstore(hashedOps, hashedOps)
            }

            PBHPayload[] memory pbhPayloads = abi.decode(opsPerAggregator[i].signature, (PBHPayload[]));
            for (uint256 j = 0; j < pbhPayloads.length; ++j) {
                address sender = opsPerAggregator[i].userOps[j].sender;
                // We now generate the signal hash from the sender, nonce, and calldata
                uint256 signalHash = abi.encodePacked(
                    sender, opsPerAggregator[i].userOps[j].nonce, opsPerAggregator[i].userOps[j].callData
                ).hashToField();

                verifyPbh(signalHash, pbhPayloads[j]);
                nullifierHashes[pbhPayloads[j].nullifierHash] = true;
                emit PBH(sender, signalHash, pbhPayloads[j]);
            }
        }

        entryPoint.handleAggregatedOps(opsPerAggregator, beneficiary);
    }

    /// @notice Validates the hashed operations is the same as the hash transiently stored.
    /// @param hashedOps The hashed operations to validate.
    function validateSignaturesCallback(bytes32 hashedOps) external view virtual onlyProxy onlyInitialized {
        assembly ("memory-safe") {
            if iszero(eq(tload(hashedOps), hashedOps)) {
                mstore(0x00, 0xf5806179) // InvalidHashedOps()
                revert(0x1c, 0x04)
            }
        }
    }

    /// @notice Executes a Priority Multicall3.
    /// @param calls The calls to execute.
    /// @param pbhPayload The PBH payload containing the proof data.
    /// @return returnData The results of the calls.
    function pbhMulticall(IMulticall3.Call3[] calldata calls, PBHPayload calldata pbhPayload)
        external
        virtual
        onlyInitialized
        onlyProxy
        nonReentrant
        returns (IMulticall3.Result[] memory returnData)
    {
        uint256 signalHash = abi.encode(msg.sender, calls).hashToField();
        verifyPbh(signalHash, pbhPayload);
        nullifierHashes[pbhPayload.nullifierHash] = true;

        returnData = IMulticall3(multicall3).aggregate3{gas: pbhGasLimit}(calls);
        emit PBH(msg.sender, signalHash, pbhPayload);

        // Check if pbh gas limit is exceeded
        if (gasleft() > pbhGasLimit) {
            revert GasLimitExceeded(gasleft());
        }

        return returnData;
    }

    /// @notice Sets the number of PBH transactions allowed per month.
    /// @param _numPbhPerMonth The number of allowed PBH transactions per month.
    function setNumPbhPerMonth(uint8 _numPbhPerMonth) external virtual onlyProxy onlyInitialized onlyOwner {
        if (_numPbhPerMonth == 0) {
            revert InvalidNumPbhPerMonth();
        }

        numPbhPerMonth = _numPbhPerMonth;
        emit NumPbhPerMonthSet(_numPbhPerMonth);
    }

    /// @dev If the World ID address is set to 0, then it is assumed that verification will take place off chain.
    /// @notice Sets the World ID instance that will be used for verifying proofs.
    /// @param _worldId The World ID instance that will be used for verifying proofs.
    function setWorldId(address _worldId) external virtual onlyProxy onlyInitialized onlyOwner {
        if (_worldId == address(0)) {
            revert AddressZero();
        }

        worldId = IWorldID(_worldId);
        emit WorldIdSet(_worldId);
    }

    /// @notice Sets the max gas limit for a PBH multicall transaction.
    /// @param _pbhGasLimit The max gas limit for a PBH multicall transaction.
    function setPBHGasLimit(uint256 _pbhGasLimit) external virtual onlyProxy onlyInitialized onlyOwner {
        if (_pbhGasLimit == 0 || _pbhGasLimit > block.gaslimit) {
            revert InvalidPBHGasLimit(_pbhGasLimit);
        }

        pbhGasLimit = _pbhGasLimit;
        emit PBHGasLimitSet(_pbhGasLimit);
    }
}
