// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {EnumerableSet} from "@openzeppelin/contracts/utils/structs/EnumerableSet.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";

import {Base} from "./abstract/Base.sol";
import {ISubsidyAccounting} from "./interfaces/ISubsidyAccounting.sol";
import {IWorldIDVerifier} from "./interfaces/IWorldIDVerifier.sol";

/// @title Subsidy Accounting Implementation V1
/// @author Worldcoin
/// @notice Per-credential, ETH-denominated transaction-fee subsidies for verified World ID
///         holders on World Chain (WIP-1002). Each `nullifier` keys a per-period subsidy
///         record holding the remaining budget, the `sessionId` for follow-up Session Proofs,
///         the set of claimed `issuerSchemaId` values, the authorized-address set, and a
///         monotonic update nonce.
/// @dev All upgrades after initial deployment must inherit this contract to avoid storage
///      collisions. Storage variables MUST NOT be reordered after deployment.
/// @custom:security-contact security@toolsforhumanity.com
contract SubsidyAccountingImplV1 is ISubsidyAccounting, Base, ReentrancyGuardTransient {
    using EnumerableSet for EnumerableSet.AddressSet;
    using EnumerableSet for EnumerableSet.UintSet;

    ///////////////////////////////////////////////////////////////////////////////
    ///                                CONSTANTS                                ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice World Chain's registered RP ID (per WIP-1001).
    uint64 public constant WORLD_CHAIN_RP_ID = 480;

    /// @notice Period length in seconds. Constant; subsidy records expire when
    ///         `block.timestamp / PERIOD_LENGTH` no longer matches the stored period.
    uint64 public constant PERIOD_LENGTH = 30 days;

    /// @notice Maximum number of distinct subsidy records (nullifiers) an address may be
    ///         simultaneously authorized under. Bounds the worst-case `consumeBudget` /
    ///         `getBudget(address)` cost and griefing surface.
    uint256 public constant MAX_NULLIFIERS_PER_ADDRESS = 16;

    /// @notice Verifier `nonce` public input committed by every proof.
    /// @dev Fixed value — see contract notes for the rationale (replay protection lives
    ///      at the contract level; the verifier nonce is structurally redundant here).
    uint256 public constant PROOF_NONCE = 0;

    /// @notice Verifier `credentialGenesisIssuedAtMin` public input committed by every proof.
    /// @dev Fixed at zero — subsidy has no recency requirement on credential issuance.
    uint256 public constant CREDENTIAL_GENESIS_ISSUED_AT_MIN = 0;

    /// @notice Domain tag bound into the `claimSubsidy` signal struct.
    bytes32 public constant CLAIM_SUBSIDY_TAG = "CLAIM_SUBSIDY";

    /// @notice Domain tag bound into the `claimAdditionalCredential` signal struct.
    bytes32 public constant CLAIM_ADDITIONAL_CREDENTIAL_TAG = "CLAIM_ADDITIONAL_CREDENTIAL";

    /// @notice Domain tag bound into the `updateAddresses` signal struct.
    bytes32 public constant UPDATE_ADDRESSES_TAG = "UPDATE_ADDRESSES";

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  TYPES                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @dev Per-period subsidy record. First three fields pack into a single storage slot
    ///      (64 + 128 + 64 = 256 bits); `sessionId` occupies the next slot.
    struct SubsidyRecord {
        uint64 periodNumber;
        uint128 remainingWei;
        uint64 updateNonce;
        uint256 sessionId;
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                             STATE VARIABLES                             ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice The World ID verifier proxy address.
    IWorldIDVerifier public worldIDVerifier;

    /// @notice Address authorized to call `consumeBudget`. Zero address disables consumption.
    address public budgetConsumer;

    /// @notice Per-credential governance-set budget allocation in Wei.
    mapping(uint64 issuerSchemaId => uint256 budgetWei) public credentialBudget;

    /// @notice Subsidy records keyed by per-period nullifier. Records lazily expire when
    ///         `periodNumber != currentPeriod()`.
    mapping(uint256 nullifier => SubsidyRecord record) internal records;

    /// @notice Per-record claimed-credentials map. `issuerSchemaId` cannot be claimed twice
    ///         under the same nullifier.
    mapping(uint256 nullifier => mapping(uint64 issuerSchemaId => bool)) internal claimed;

    /// @notice Per-record authorized-address set.
    mapping(uint256 nullifier => EnumerableSet.AddressSet) internal authorized;

    /// @notice Reverse index from authorized account address to the set of `nullifier`
    ///         records it may draw budget from. Capped at `MAX_NULLIFIERS_PER_ADDRESS`.
    mapping(address account => EnumerableSet.UintSet) internal nullifiersOf;

    ///////////////////////////////////////////////////////////////////////////////
    ///                                MODIFIERS                                ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @dev Restricts the caller to the configured `budgetConsumer`.
    modifier onlyBudgetConsumer() {
        if (msg.sender != budgetConsumer) revert NotBudgetConsumer();
        _;
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              CONSTRUCTION                               ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Locks the implementation. State setup happens in `initialize` against the proxy.
    constructor() {
        _disableInitializers();
    }

    /// @notice Initializes the proxy.
    /// @param worldIDVerifier_ The World ID 4.0 verifier address. Must not be zero.
    /// @param owner_ The owner that will gain admin privileges via `Ownable2Step`.
    function initialize(IWorldIDVerifier worldIDVerifier_, address owner_) external reinitializer(1) {
        if (address(worldIDVerifier_) == address(0)) revert AddressZero();
        if (owner_ == address(0)) revert AddressZero();

        __Base_init(owner_);
        worldIDVerifier = worldIDVerifier_;

        emit SubsidyAccountingImplInitialized(worldIDVerifier_, owner_);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              EXTERNAL API                               ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @inheritdoc ISubsidyAccounting
    function claimSubsidy(uint256, uint256, address[] calldata, ClaimItem[] calldata)
        external
        virtual
        onlyProxy
        nonReentrant
    {
        revert NotImplemented();
    }

    /// @inheritdoc ISubsidyAccounting
    function claimAdditionalCredential(uint256, uint256, uint256[2] calldata, uint256[5] calldata)
        external
        virtual
        onlyProxy
        nonReentrant
    {
        revert NotImplemented();
    }

    /// @inheritdoc ISubsidyAccounting
    function updateAddresses(
        uint256,
        uint256,
        address[] calldata,
        address[] calldata,
        uint256[2] calldata,
        uint256[5] calldata
    ) external virtual onlyProxy nonReentrant {
        revert NotImplemented();
    }

    /// @inheritdoc ISubsidyAccounting
    function consumeBudget(address, uint256, uint256) external virtual onlyProxy nonReentrant onlyBudgetConsumer {
        revert NotImplemented();
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  VIEWS                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @inheritdoc ISubsidyAccounting
    function getBudget(uint256 nullifier) external view virtual onlyProxy returns (uint256 remainingWei) {
        SubsidyRecord storage r = records[nullifier];
        if (r.periodNumber != currentPeriod()) return 0;
        return r.remainingWei;
    }

    /// @inheritdoc ISubsidyAccounting
    function getBudget(address account) external view virtual onlyProxy returns (uint256 remainingWei) {
        EnumerableSet.UintSet storage nullifiers = nullifiersOf[account];
        uint64 period = currentPeriod();
        uint256 len = nullifiers.length();
        for (uint256 i = 0; i < len; ++i) {
            SubsidyRecord storage r = records[nullifiers.at(i)];
            if (r.periodNumber == period) {
                remainingWei += r.remainingWei;
            }
        }
    }

    /// @inheritdoc ISubsidyAccounting
    function isAuthorized(address account, uint256 nullifier) external view virtual onlyProxy returns (bool) {
        if (records[nullifier].periodNumber != currentPeriod()) return false;
        return authorized[nullifier].contains(account);
    }

    /// @inheritdoc ISubsidyAccounting
    function getNullifiers(address account) external view virtual onlyProxy returns (uint256[] memory) {
        return nullifiersOf[account].values();
    }

    /// @inheritdoc ISubsidyAccounting
    function isClaimed(uint256 nullifier, uint256 issuerSchemaId) external view virtual onlyProxy returns (bool) {
        if (issuerSchemaId > type(uint64).max) return false;
        return claimed[nullifier][uint64(issuerSchemaId)];
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  ADMIN                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @inheritdoc ISubsidyAccounting
    function setWorldIDVerifier(IWorldIDVerifier worldIDVerifier_) external virtual onlyProxy onlyOwner {
        if (address(worldIDVerifier_) == address(0)) revert AddressZero();
        worldIDVerifier = worldIDVerifier_;
        emit WorldIDVerifierSet(address(worldIDVerifier_));
    }

    /// @inheritdoc ISubsidyAccounting
    function setBudgetConsumer(address budgetConsumer_) external virtual onlyProxy onlyOwner {
        budgetConsumer = budgetConsumer_;
        emit BudgetConsumerSet(budgetConsumer_);
    }

    /// @inheritdoc ISubsidyAccounting
    function setCredentialBudget(uint256 issuerSchemaId, uint256 budgetWei) external virtual onlyProxy onlyOwner {
        if (issuerSchemaId > type(uint64).max) revert IssuerSchemaIdOverflow(issuerSchemaId);
        uint64 schemaId = uint64(issuerSchemaId);
        credentialBudget[schemaId] = budgetWei;
        emit CredentialBudgetSet(schemaId, budgetWei);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                INTERNAL                                 ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @dev Current subsidy period derived from `block.timestamp`.
    function currentPeriod() internal view returns (uint64) {
        return uint64(block.timestamp / PERIOD_LENGTH);
    }
}
