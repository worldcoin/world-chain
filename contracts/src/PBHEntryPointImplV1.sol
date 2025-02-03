// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IWorldID} from "@world-id-contracts/interfaces/IWorldID.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";
import {IPBHEntryPoint} from "./interfaces/IPBHEntryPoint.sol";
import {IMulticall3} from "./interfaces/IMulticall3.sol";
import {ByteHasher} from "./lib/ByteHasher.sol";
import {PBHExternalNullifier} from "./lib/PBHExternalNullifier.sol";
import {WorldIDImpl} from "@world-id-contracts/abstract/WorldIDImpl.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import "@BokkyPooBahsDateTimeLibrary/BokkyPooBahsDateTimeLibrary.sol";

/// @title PBH Entry Point Implementation V1
/// @author Worldcoin
/// @notice This contract is an implementation of the PBH Entry Point.
/// It is used to verify the signatures in a PBH bundle, and relay bundles to the EIP-4337 Entry Point.
/// @dev All upgrades to the PBHEntryPoint after initial deployment must inherit this contract to avoid storage collisions.
/// Also note that that storage variables must not be reordered after deployment otherwise storage collisions will occur.
/// @custom:security-contact security@toolsforhumanity.com
contract PBHEntryPointImplV1 is IPBHEntryPoint, WorldIDImpl, ReentrancyGuardTransient {
    using ByteHasher for bytes;

    ///////////////////////////////////////////////////////////////////////////////
    ///                             STATE VARIABLES                             ///
    //////////////////////////////////////////////////////////////////////////////

    /// @dev The World ID instance that will be used for verifying proofs
    IWorldID public worldId;

    /// @dev The EntryPoint where Aggregated PBH Bundles will be proxied to.
    IEntryPoint public entryPoint;

    /// @notice The number of PBH transactions alloted to each World ID per month, 0 indexed.
    ///         For example, if `numPbhPerMonth` is 29, a user can submit 30 PBH txs
    uint8 public numPbhPerMonth;

    /// @notice Address of the Multicall3 implementation.
    address internal _multicall3;

    /// @dev Whether a nullifier hash has been used already. Used to guarantee an action is only performed once by a single person
    mapping(uint256 => bool) public nullifierHashes;

    /// @notice The gas limit for a PBH multicall transaction
    uint256 public pbhGasLimit;

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  Events                                ///
    //////////////////////////////////////////////////////////////////////////////

    /// @notice Emitted when the contract is initialized.
    ///
    /// @param worldId The World ID instance that will be used for verifying proofs.
    /// @param entryPoint The ERC-4337 Entry Point.
    /// @param numPbhPerMonth The number of allowed PBH transactions per month.
    /// @param multicall3 Address of the Multicall3 implementation.
    /// @param pbhGasLimit The gas limit for a PBH multicall transaction.
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
    error GasLimitExceeded(uint256 gasLeft, uint256 gasLimit);

    /// @notice Thrown when setting the gas limit for a PBH multicall to 0
    error InvalidPBHGasLimit(uint256 gasLimit);

    /// @notice Thrown when the length of PBHPayloads on the aggregated signature is not equivalent to the amount of UserOperations.
    error InvalidAggregatedSignature(uint256 payloadsLength, uint256 userOpsLength);

    ///////////////////////////////////////////////////////////////////////////////
    ///                               FUNCTIONS                                 ///
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
    ///      with upgrades based upon this contract. Be aware that there are only 255 (parameter is `uint8` and first value is 1)
    ///      initialisations allowed, so decide carefully when to use them. Many cases can safely be
    ///      replaced by use of setters.
    /// @dev This function is explicitly not virtual as it does not make sense to override even when
    ///      upgrading. Create a separate initializer function instead.
    ///
    /// @param _worldId The World ID instance that will be used for verifying proofs. If set to the
    ///        0 addess, then it will be assumed that verification will take place off chain.
    /// @param _entryPoint The ERC-4337 Entry Point.
    /// @param _numPbhPerMonth The number of allowed PBH transactions per month.
    /// @param multicall3 Address of the Multicall3 implementation.
    /// @param _pbhGasLimit The gas limit for a PBH multicall transaction.
    ///
    /// @custom:reverts string If called more than once at the same initialisation number.
    function initialize(
        IWorldID _worldId,
        IEntryPoint _entryPoint,
        uint8 _numPbhPerMonth,
        address multicall3,
        uint256 _pbhGasLimit
    ) external reinitializer(1) {
        if (address(_entryPoint) == address(0) || multicall3 == address(0)) {
            revert AddressZero();
        }

        if (_numPbhPerMonth == 0) {
            revert InvalidNumPbhPerMonth();
        }

        __WorldIDImpl_init();

        worldId = _worldId;
        entryPoint = _entryPoint;
        numPbhPerMonth = _numPbhPerMonth;
        _multicall3 = multicall3;

        if (_pbhGasLimit == 0 || _pbhGasLimit > block.gaslimit) {
            revert InvalidPBHGasLimit(_pbhGasLimit);
        }

        pbhGasLimit = _pbhGasLimit;
        // Say that the contract is initialized.
        __setInitialized();
        emit PBHEntryPointImplInitialized(_worldId, _entryPoint, _numPbhPerMonth, multicall3, _pbhGasLimit);
    }

    /// @notice Verifies a PBH payload.
    /// @param signalHash The signal hash associated with the PBH payload.
    /// @param pbhPayload The PBH payload containing the proof data.
    function verifyPbh(uint256 signalHash, PBHPayload memory pbhPayload)
        public
        view
        virtual
        onlyProxy
        onlyInitialized
    {
        _verifyPbh(signalHash, pbhPayload);
    }

    /// @notice Verifies a PBH payload.
    /// @param signalHash The signal hash associated with the PBH payload.
    /// @param pbhPayload The PBH payload containing the proof data.
    function _verifyPbh(uint256 signalHash, PBHPayload memory pbhPayload) internal view {
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
                if tload(hashedOps) {
                    mstore(0x00, 0x5e75ad06) // StorageCollision()
                    revert(0x1c, 0x04)
                }

                tstore(hashedOps, hashedOps)
            }

            PBHPayload[] memory pbhPayloads = abi.decode(opsPerAggregator[i].signature, (PBHPayload[]));
            require(
                pbhPayloads.length == opsPerAggregator[i].userOps.length,
                InvalidAggregatedSignature(pbhPayloads.length, opsPerAggregator[i].userOps.length)
            );
            for (uint256 j = 0; j < pbhPayloads.length; ++j) {
                address sender = opsPerAggregator[i].userOps[j].sender;
                // We now generate the signal hash from the sender, nonce, and calldata
                uint256 signalHash = abi.encodePacked(
                    sender, opsPerAggregator[i].userOps[j].nonce, opsPerAggregator[i].userOps[j].callData
                ).hashToField();

                _verifyPbh(signalHash, pbhPayloads[j]);
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
        onlyProxy
        onlyInitialized
        nonReentrant
        returns (IMulticall3.Result[] memory returnData)
    {
        if (gasleft() > pbhGasLimit) {
            revert GasLimitExceeded(gasleft(), pbhGasLimit);
        }

        uint256 signalHash = abi.encode(msg.sender, calls).hashToField();
        _verifyPbh(signalHash, pbhPayload);
        nullifierHashes[pbhPayload.nullifierHash] = true;

        returnData = IMulticall3(_multicall3).aggregate3(calls);
        emit PBH(msg.sender, signalHash, pbhPayload);
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
