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

    /// @dev Pre-image of `claimSubsidy`'s `signalHash` public input. ABI-encoded layout is
    ///      pinned by WIP-1002 § "Signal Binding"; off-chain reproducers must match.
    struct ClaimSubsidySignal {
        bytes32 tag;
        uint256 sessionId;
        address[] addAddresses;
        address msgSender;
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
    function claimSubsidy(
        uint256 nullifier,
        uint256 sessionId,
        address[] calldata addAddresses,
        ClaimItem[] calldata items
    ) external virtual onlyProxy nonReentrant {
        if (items.length == 0) revert EmptyItems();

        uint64 period = currentPeriod();
        if (records[nullifier].periodNumber == period) revert RecordAlreadyExists();

        uint256 signalHash = uint256(
            keccak256(
                abi.encode(
                    ClaimSubsidySignal({
                        tag: CLAIM_SUBSIDY_TAG,
                        sessionId: sessionId,
                        addAddresses: addAddresses,
                        msgSender: msg.sender
                    })
                )
            )
        ) >> 8;

        uint256 totalBudget = _accrueClaimedCredentials(nullifier, items);
        if (totalBudget > type(uint128).max) revert BudgetOverflow(totalBudget);

        records[nullifier] = SubsidyRecord({
            periodNumber: period,
            remainingWei: uint128(totalBudget),
            updateNonce: 0,
            sessionId: sessionId
        });

        _addAddresses(nullifier, addAddresses);

        emit SubsidyClaimed(nullifier, sessionId, period, totalBudget);

        _verifyClaimItems(nullifier, period, signalHash, items);
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

    /// @dev Validate every `items[i].issuerSchemaId`, mark each as claimed under `nullifier`,
    ///      and return the summed configured budget. Reverts on overflow, duplicates, or an
    ///      already-claimed credential. Pulled out of `claimSubsidy` to keep its frame within
    ///      the 16-local stack limit.
    function _accrueClaimedCredentials(uint256 nullifier, ClaimItem[] calldata items)
        internal
        returns (uint256 totalBudget)
    {
        uint256 itemsLen = items.length;
        for (uint256 i = 0; i < itemsLen; ++i) {
            uint256 rawSchemaId = items[i].issuerSchemaId;
            if (rawSchemaId > type(uint64).max) revert IssuerSchemaIdOverflow(rawSchemaId);
            uint64 schemaId = uint64(rawSchemaId);
            if (claimed[nullifier][schemaId]) revert DuplicateIssuerSchemaId(schemaId);
            claimed[nullifier][schemaId] = true;
            totalBudget += credentialBudget[schemaId];
        }
    }

    /// @dev Verify every `items[i].proof` as a Uniqueness Proof for `period_proof || period`
    ///      against the shared `signalHash`. Reverts atomically if any item is rejected.
    ///      Verifier inputs `nonce`, `expiresAtMin`, `credentialGenesisIssuedAtMin` are supplied
    ///      deterministically from `PROOF_NONCE`, `period * PERIOD_LENGTH`, and
    ///      `CREDENTIAL_GENESIS_ISSUED_AT_MIN`.
    function _verifyClaimItems(uint256 nullifier, uint64 period, uint256 signalHash, ClaimItem[] calldata items)
        internal
        view
    {
        uint256 action = uint256(keccak256(abi.encodePacked("period_proof", period))) >> 8;
        uint64 expiresAtMin = period * PERIOD_LENGTH;
        uint256 itemsLen = items.length;
        for (uint256 i = 0; i < itemsLen; ++i) {
            worldIDVerifier.verify(
                nullifier,
                action,
                WORLD_CHAIN_RP_ID,
                PROOF_NONCE,
                signalHash,
                expiresAtMin,
                uint64(items[i].issuerSchemaId),
                CREDENTIAL_GENESIS_ISSUED_AT_MIN,
                items[i].proof
            );
        }
    }

    /// @dev Add `addrs` to the authorized set of `nullifier` and the reverse index.
    ///      Reverts on zero address, duplicate within the existing authorised set, or when
    ///      adding the new authorisation would push an account past
    ///      `MAX_NULLIFIERS_PER_ADDRESS`. Shared by `claimSubsidy` and `updateAddresses`.
    function _addAddresses(uint256 nullifier, address[] calldata addrs) internal {
        EnumerableSet.AddressSet storage authSet = authorized[nullifier];
        uint256 len = addrs.length;
        for (uint256 i = 0; i < len; ++i) {
            address acct = addrs[i];
            if (acct == address(0)) revert AddressZero();
            if (!authSet.add(acct)) revert DuplicateAuthorizedAddress(acct);

            EnumerableSet.UintSet storage owned = nullifiersOf[acct];
            if (!owned.contains(nullifier) && owned.length() >= MAX_NULLIFIERS_PER_ADDRESS) {
                revert TooManyNullifiers(acct);
            }
            owned.add(nullifier);
        }
    }
}
