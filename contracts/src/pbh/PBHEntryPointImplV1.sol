// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IWorldID} from "@world-id-contracts/interfaces/IWorldID.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";
import {PackedUserOperation} from "@account-abstraction/contracts/interfaces/PackedUserOperation.sol";
import {UserOperationLib} from "@account-abstraction/contracts/core/UserOperationLib.sol";
import {IPBHEntryPoint} from "./interfaces/IPBHEntryPoint.sol";
import {ByteHasher} from "./libraries/ByteHasher.sol";
import {PBHExternalNullifier} from "./libraries/PBHExternalNullifier.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import "@BokkyPooBahsDateTimeLibrary/BokkyPooBahsDateTimeLibrary.sol";
import {Base} from "../abstract/Base.sol";

/// @title PBH Entry Point Implementation V1
/// @author Worldcoin
/// @notice This contract is an implementation of the PBH Entry Point.
/// It is used to verify the signatures in a PBH bundle, and relay bundles to the EIP-4337 Entry Point.
/// @dev All upgrades to the PBHEntryPoint after initial deployment must inherit this contract to avoid storage collisions.
/// Also note that that storage variables must not be reordered after deployment otherwise storage collisions will occur.
/// @custom:security-contact security@toolsforhumanity.com
contract PBHEntryPointImplV1 is IPBHEntryPoint, Base, ReentrancyGuardTransient {
    using ByteHasher for bytes;
    using UserOperationLib for PackedUserOperation;

    ///////////////////////////////////////////////////////////////////////////////
    ///                             STATE VARIABLES                             ///
    //////////////////////////////////////////////////////////////////////////////

    /// @dev The World ID instance that will be used for verifying proofs
    IWorldID public worldId;

    /// @dev The EntryPoint where Aggregated PBH Bundles will be proxied to.
    IEntryPoint public entryPoint;

    /// @notice The number of PBH transactions alloted to each World ID per month, 0 indexed.
    ///         For example, if `numPbhPerMonth` is 29, a user can submit 30 PBH txs
    uint16 public numPbhPerMonth;

    /// @dev Whether a nullifier hash has been used already. Used to guarantee an action is only performed once by a single person
    mapping(uint256 nullifierHash => uint256 blockNumber) public nullifierHashes;

    /// @notice A mapping of builder public keys to their respective authorization status in the contract.
    ///
    /// @dev Authorized builders are expected to back run built blocks with the nullifier hashes spent
    ///      within all PBH Proofs in the block.
    mapping(address builder => bool authorized) public authorizedBuilder;

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
    /// @param pbhGasLimit The gas limit for a PBH multicall transaction.
    /// @param authorizedBuilders The addresses of the builders that are authorized.
    /// @param owner The owner of the contract.
    event PBHEntryPointImplInitialized(
        IWorldID indexed worldId,
        IEntryPoint indexed entryPoint,
        uint16 indexed numPbhPerMonth,
        uint256 pbhGasLimit,
        address[] authorizedBuilders,
        address owner
    );

    /// @notice Emitted once for each successful PBH verification.
    ///
    /// @param sender The sender of this particular transaction or UserOp.
    /// @param userOpHash The hash of the UserOperation that contains the PBHPayload.
    /// @param payload The zero-knowledge proof that demonstrates the claimer is registered with World ID.
    event PBH(address indexed sender, bytes32 indexed userOpHash, PBHPayload payload);

    /// @notice Emitted when the World ID address is set.
    ///
    /// @param worldId The World ID instance that will be used for verifying proofs.
    event WorldIdSet(address indexed worldId);

    /// @notice Emitted when the number of PBH transactions allowed per month is set.
    ///
    /// @param numPbhPerMonth The number of allowed PBH transactions per month.
    event NumPbhPerMonthSet(uint16 indexed numPbhPerMonth);

    /// @notice Emitted when setting the PBH gas limit.
    ///
    /// @param pbhGasLimit The gas limit for a PBH multicall transaction.
    event PBHGasLimitSet(uint256 indexed pbhGasLimit);

    /// @notice Emitted when the nullifier hashes are spent.
    ///
    /// @param builder The address of the builder that spent the nullifier hashes.
    /// @param nullifierHashes The nullifier hashes that were spent.
    event NullifierHashesSpent(address indexed builder, uint256[] nullifierHashes);

    /// @notice Emitted when the builder is authorized to build blocks.
    ///
    /// @param builder The address of the builder that is authorized.
    event BuilderAuthorized(address indexed builder);

    /// @notice Emitted when the builder is deauthorized to build blocks.
    ///
    /// @param builder The address of the builder that is deauthorized.
    event BuilderDeauthorized(address indexed builder);

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  ERRORS                                ///
    //////////////////////////////////////////////////////////////////////////////

    /// @notice Thrown when attempting to reuse a nullifier
    /// @param signalHash The signal hash associated with the PBH payload.
    error InvalidNullifier(uint256 nullifierHash, uint256 signalHash);

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

    /// @notice Thrown when the builder is not authorized to build blocks
    error UnauthorizedBuilder();

    /// @notice Thrown when there are no authorized builders
    error InvalidAuthorizedBuilders();

    ///////////////////////////////////////////////////////////////////////////////
    ///                               FUNCTIONS                                 ///
    ///////////////////////////////////////////////////////////////////////////////

    modifier onlyBuilder() {
        if (!authorizedBuilder[msg.sender]) {
            revert UnauthorizedBuilder();
        }
        _;
    }

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
    ///        0 address, then it will be assumed that verification will take place off chain.
    /// @param _entryPoint The ERC-4337 Entry Point.
    /// @param _numPbhPerMonth The number of allowed PBH transactions per month.
    /// @param _pbhGasLimit The gas limit for a PBH multicall transaction.
    /// @param _owner The owner of the contract.
    ///
    /// @custom:reverts string If called more than once at the same initialisation number.
    function initialize(
        IWorldID _worldId,
        IEntryPoint _entryPoint,
        uint16 _numPbhPerMonth,
        uint256 _pbhGasLimit,
        address[] memory _authorizedBuilders,
        address _owner
    ) external reinitializer(1) {
        if (address(_entryPoint) == address(0)) {
            revert AddressZero();
        }

        if (_numPbhPerMonth == 0) {
            revert InvalidNumPbhPerMonth();
        }

        if (_authorizedBuilders.length == 0) {
            revert InvalidAuthorizedBuilders();
        }

        for (uint256 i = 0; i < _authorizedBuilders.length; ++i) {
            if (_authorizedBuilders[i] == address(0)) {
                revert AddressZero();
            }
            authorizedBuilder[_authorizedBuilders[i]] = true;
        }

        __Base_init(_owner);

        worldId = _worldId;
        entryPoint = _entryPoint;
        numPbhPerMonth = _numPbhPerMonth;

        if (_pbhGasLimit == 0 || _pbhGasLimit > block.gaslimit) {
            revert InvalidPBHGasLimit(_pbhGasLimit);
        }

        pbhGasLimit = _pbhGasLimit;

        emit PBHEntryPointImplInitialized(
            _worldId, _entryPoint, _numPbhPerMonth, _pbhGasLimit, _authorizedBuilders, _owner
        );
    }

    /// @notice Verifies a PBH payload.
    /// @param signalHash The signal hash associated with the PBH payload.
    /// @param pbhPayload The PBH payload containing the proof data.
    function verifyPbh(uint256 signalHash, PBHPayload memory pbhPayload) public view virtual onlyProxy {
        _verifyPbh(signalHash, pbhPayload);
    }

    /// @notice Verifies a PBH payload.
    /// @param signalHash The signal hash associated with the PBH payload.
    /// @param pbhPayload The PBH payload containing the proof data.
    function _verifyPbh(uint256 signalHash, PBHPayload memory pbhPayload) internal view {
        // First, we make sure this nullifier has not been used before.
        if (nullifierHashes[pbhPayload.nullifierHash] != 0) {
            revert InvalidNullifier(pbhPayload.nullifierHash, signalHash);
        }

        // Verify the external nullifier
        PBHExternalNullifier.verify(pbhPayload.pbhExternalNullifier, numPbhPerMonth, signalHash);

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
    ) external virtual onlyProxy nonReentrant {
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
                bytes32 userOpHash = getUserOpHash(opsPerAggregator[i].userOps[j]);
                emit PBH(sender, userOpHash, pbhPayloads[j]);
            }
        }

        entryPoint.handleAggregatedOps(opsPerAggregator, beneficiary);
    }

    /// @notice Validates the hashed operations is the same as the hash transiently stored.
    /// @param hashedOps The hashed operations to validate.
    function validateSignaturesCallback(bytes32 hashedOps) external view virtual onlyProxy {
        assembly ("memory-safe") {
            if iszero(eq(tload(hashedOps), hashedOps)) {
                mstore(0x00, 0xf5806179) // InvalidHashedOps()
                revert(0x1c, 0x04)
            }
        }
    }

    /// @notice Sets the number of PBH transactions allowed per month.
    /// @param _numPbhPerMonth The number of allowed PBH transactions per month.
    function setNumPbhPerMonth(uint16 _numPbhPerMonth) external virtual onlyProxy onlyOwner {
        if (_numPbhPerMonth == 0) {
            revert InvalidNumPbhPerMonth();
        }

        numPbhPerMonth = _numPbhPerMonth;
        emit NumPbhPerMonthSet(_numPbhPerMonth);
    }

    /// @dev If the World ID address is set to 0, then it is assumed that verification will take place off chain.
    /// @notice Sets the World ID instance that will be used for verifying proofs.
    /// @param _worldId The World ID instance that will be used for verifying proofs.
    function setWorldId(address _worldId) external virtual onlyProxy onlyOwner {
        worldId = IWorldID(_worldId);
        emit WorldIdSet(_worldId);
    }

    /// @notice Sets the max gas limit for a PBH multicall transaction.
    /// @param _pbhGasLimit The max gas limit for a PBH multicall transaction.
    function setPBHGasLimit(uint256 _pbhGasLimit) external virtual onlyProxy onlyOwner {
        if (_pbhGasLimit == 0 || _pbhGasLimit > block.gaslimit) {
            revert InvalidPBHGasLimit(_pbhGasLimit);
        }

        pbhGasLimit = _pbhGasLimit;
        emit PBHGasLimitSet(_pbhGasLimit);
    }

    /// @notice Adds a builder to the list of authorized builders.
    /// @param builder The address of the builder to authorize.
    function addBuilder(address builder) external virtual onlyProxy onlyOwner {
        if (builder == address(0)) {
            revert AddressZero();
        }

        authorizedBuilder[builder] = true;
        emit BuilderAuthorized(builder);
    }

    /// @notice Removes a builder from the list of authorized builders.
    /// @param builder The address of the builder to deauthorize.
    function removeBuilder(address builder) external virtual onlyProxy onlyOwner {
        delete authorizedBuilder[builder];
        emit BuilderDeauthorized(builder);
    }

    /// @notice Allows a builder to spend all nullifiers within PBH blockspace.
    /// @param _nullifierHashes The nullifier hashes to spend.
    function spendNullifierHashes(uint256[] calldata _nullifierHashes) external virtual onlyProxy onlyBuilder {
        for (uint256 i = 0; i < _nullifierHashes.length; ++i) {
            nullifierHashes[_nullifierHashes[i]] = block.number;
        }

        emit NullifierHashesSpent(msg.sender, _nullifierHashes);
    }

    /// @notice Returns a hash of the UserOperation.
    /// @param userOp The UserOperation to hash.
    function getUserOpHash(PackedUserOperation calldata userOp) public view virtual returns (bytes32 hash) {
        hash = keccak256(abi.encode(userOp.hash(), address(entryPoint), block.chainid));
    }

    /// @notice Returns the index of the first unspent nullifier hash in the given list.
    /// @notice This function assumes the input array represents nullifier hashes that are
    /// @notice generated from the same sempahore key and monotonically increasing nonces.
    /// @param hashes The list of nullifier hashes to search through.
    /// @return The index of the first unspent nullifier hash in the given list.
    /// @dev Returns -1 if no unspent nullifier hash is found.
    function getFirstUnspentNullifierHash(uint256[] calldata hashes) public view virtual returns (int256) {
        for (uint256 i = 0; i < hashes.length; ++i) {
            if (nullifierHashes[hashes[i]] == 0) {
                return int256(i);
            }
        }
        return -1;
    }

    /// @notice Returns all indexes of unspent nullifier hashes in the given list.
    /// @param hashes The list of nullifier hashes to search through.
    /// @return The indexes of the unspent nullifier hashes in the given list.
    /// @dev Returns an empty array if no unspent nullifier hashes are found.
    function getUnspentNullifierHashes(uint256[] calldata hashes) public view virtual returns (uint256[] memory) {
        uint256[] memory tempIndexes = new uint256[](hashes.length);
        uint256 unspentCount = 0;

        for (uint256 i = 0; i < hashes.length; ++i) {
            if (nullifierHashes[hashes[i]] == 0) {
                tempIndexes[unspentCount] = i;
                unspentCount++;
            }
        }

        uint256[] memory unspentIndexes = new uint256[](unspentCount);
        for (uint256 i = 0; i < unspentCount; ++i) {
            unspentIndexes[i] = tempIndexes[i];
        }

        return unspentIndexes;
    }
}
