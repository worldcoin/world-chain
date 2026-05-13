// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

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
    ///////////////////////////////////////////////////////////////////////////////
    ///                                CONSTANTS                                ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice World Chain's registered RP ID (per WIP-1001).
    uint64 public constant WORLD_CHAIN_RP_ID = 480;

    /// @notice Period length in seconds. Subsidy records are per-period and expire
    ///         structurally via the action key derivation; see `currentAction`.
    uint64 public constant PERIOD_LENGTH = 30 days;

    /// @notice Verifier `nonce` public input committed by every proof.
    /// @dev Fixed value — see contract notes for the rationale (replay protection lives
    ///      at the contract level; the verifier nonce is structurally redundant here).
    uint256 public constant PROOF_NONCE = 0;

    /// @notice Verifier `credentialGenesisIssuedAtMin` public input committed by every proof.
    /// @dev Fixed at zero — subsidy has no recency requirement on credential issuance.
    uint256 public constant CREDENTIAL_GENESIS_ISSUED_AT_MIN = 0;

    /// @notice Verifier `issuerSchemaId` public input bound into Session Proofs that are not
    ///         claiming a credential (currently `setAuthorized`). Session Proofs prove
    ///         continuity with the stored `sessionId`; the schema field is committed-but-unused
    ///         and pinned here to a deterministic sentinel so authenticator and contract agree.
    uint64 public constant SESSION_PROOF_SCHEMA_ID = 0;

    /// @notice Domain tag bound into the `claimSubsidy` signal struct.
    bytes32 public constant CLAIM_SUBSIDY_TAG = "CLAIM_SUBSIDY";

    /// @notice Domain tag bound into the `claimAdditionalCredential` signal struct.
    bytes32 public constant CLAIM_ADDITIONAL_CREDENTIAL_TAG = "CLAIM_ADDITIONAL_CREDENTIAL";

    /// @notice Domain tag bound into the `setAuthorized` signal struct.
    bytes32 public constant SET_AUTHORIZED_TAG = "SET_AUTHORIZED";

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  TYPES                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @dev Per-period subsidy record. First three fields pack into a single storage slot
    ///      (64 + 128 + 64 = 256 bits); `sessionId` occupies the next slot.
    /// @custom:pinned This struct is part of the WIP-1002 protocol commitment. The revm
    ///                debit hook reads and writes `records[action][nullifier]` via the
    ///                pinned slot derivation, so field offsets, types, and packing MUST NOT
    ///                change on upgrade. Append-only on the underlying mapping slot too.
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

    /// @dev Pre-image of `claimAdditionalCredential`'s `signalHash` public input.
    struct ClaimAdditionalCredentialSignal {
        bytes32 tag;
        uint256 nullifier;
        address msgSender;
    }

    /// @dev Pre-image of `setAuthorized`'s `signalHash` public input.
    struct SetAuthorizedSignal {
        bytes32 tag;
        uint256 nullifier;
        uint64 nonce;
        address[] newSet;
        address msgSender;
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                             STATE VARIABLES                             ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice The World ID verifier proxy address.
    IWorldIDVerifier public worldIDVerifier;

    /// @notice Monotonic mixer bumped on `setWorldIDVerifier`. Folded into `currentAction`
    ///         so a verifier swap structurally invalidates every in-flight per-period record
    ///         without touching any individual slot.
    uint64 internal _registrationVersion;

    /// @notice Per-credential governance-set budget allocation in Wei.
    mapping(uint64 issuerSchemaId => uint256 budgetWei) public credentialBudget;

    /// @notice Subsidy records keyed by `(action, nullifier)`. Per-period reset is structural —
    ///         `action` folds in `currentPeriod()` and `_registrationVersion`, so old-period
    ///         and pre-verifier-swap slots are unreachable from current-action lookups.
    /// @custom:pinned This mapping slot is part of the WIP-1002 protocol commitment.
    mapping(uint256 action => mapping(uint256 nullifier => SubsidyRecord record)) internal records;

    /// @notice Per-record claimed-credentials map. `issuerSchemaId` cannot be claimed twice
    ///         under the same `(action, nullifier)`.
    mapping(uint256 action => mapping(uint256 nullifier => mapping(uint64 issuerSchemaId => bool))) internal claimed;

    /// @notice Per-record authorized-address set, ordered by insertion. Plain `address[]`
    ///         (not `EnumerableSet`) to avoid coupling the protocol-relevant layout to an
    ///         OpenZeppelin library version.
    mapping(uint256 action => mapping(uint256 nullifier => address[])) internal authorized;

    /// @notice Reverse index from authorized account to its authorised nullifiers, ordered by
    ///         insertion. The revm debit hook walks this slot.
    /// @custom:pinned This mapping slot is part of the WIP-1002 protocol commitment.
    mapping(uint256 action => mapping(address account => uint256[])) internal nullifiersOf;

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
        uint256 action = _actionForPeriod(period);
        if (records[action][nullifier].periodNumber == period) revert RecordAlreadyExists();

        uint256 signalHash = uint256(
            keccak256(
                abi.encode(
                    ClaimSubsidySignal({
                        tag: CLAIM_SUBSIDY_TAG, sessionId: sessionId, addAddresses: addAddresses, msgSender: msg.sender
                    })
                )
            )
        ) >> 8;

        uint256 totalBudget = _accrueClaimedCredentials(action, nullifier, items);
        if (totalBudget > type(uint128).max) revert BudgetOverflow(totalBudget);

        records[action][nullifier] = SubsidyRecord({
            periodNumber: period, remainingWei: uint128(totalBudget), updateNonce: 0, sessionId: sessionId
        });

        _addAddresses(action, nullifier, addAddresses);

        emit SubsidyClaimed(nullifier, sessionId, period, totalBudget);

        _verifyClaimItems(nullifier, period, action, signalHash, items);
    }

    /// @inheritdoc ISubsidyAccounting
    function claimAdditionalCredential(uint256, uint64, uint256, uint256, uint256[5] calldata)
        external
        virtual
        onlyProxy
        nonReentrant
    {
        revert NotImplemented();
    }

    /// @inheritdoc ISubsidyAccounting
    function setAuthorized(
        uint256 nullifier,
        uint64 nonce,
        address[] calldata newSet,
        uint256 sessionNullifier,
        uint256 sessionAction,
        uint256[5] calldata proof
    ) external virtual onlyProxy nonReentrant {
        uint64 period = currentPeriod();
        uint256 action = _actionForPeriod(period);
        SubsidyRecord storage r = records[action][nullifier];
        if (r.periodNumber != period) revert RecordDoesNotExist();
        if (nonce != r.updateNonce) revert StaleUpdateNonce(nonce, r.updateNonce);

        uint256 signalHash = uint256(
            keccak256(
                abi.encode(
                    SetAuthorizedSignal({
                        tag: SET_AUTHORIZED_TAG,
                        nullifier: nullifier,
                        nonce: nonce,
                        newSet: newSet,
                        msgSender: msg.sender
                    })
                )
            )
        ) >> 8;

        uint256[2] memory sessionInputs = [sessionNullifier, sessionAction];
        worldIDVerifier.verifySession(
            WORLD_CHAIN_RP_ID,
            PROOF_NONCE,
            signalHash,
            period * PERIOD_LENGTH,
            SESSION_PROOF_SCHEMA_ID,
            CREDENTIAL_GENESIS_ISSUED_AT_MIN,
            r.sessionId,
            sessionInputs,
            proof
        );

        _replaceAuthorized(action, nullifier, newSet);
        ++r.updateNonce;
        emit AuthorizedSetUpdated(nullifier, newSet);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  VIEWS                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @inheritdoc ISubsidyAccounting
    function getBudget(uint256 nullifier) external view virtual onlyProxy returns (uint256 remainingWei) {
        return records[currentAction()][nullifier].remainingWei;
    }

    /// @inheritdoc ISubsidyAccounting
    function getBudget(address account) external view virtual onlyProxy returns (uint256 remainingWei) {
        uint256 action = currentAction();
        uint256[] storage nullifiers = nullifiersOf[action][account];
        uint256 len = nullifiers.length;
        for (uint256 i = 0; i < len; ++i) {
            remainingWei += records[action][nullifiers[i]].remainingWei;
        }
    }

    /// @inheritdoc ISubsidyAccounting
    function isAuthorized(address account, uint256 nullifier) external view virtual onlyProxy returns (bool) {
        address[] storage authSet = authorized[currentAction()][nullifier];
        uint256 len = authSet.length;
        for (uint256 i = 0; i < len; ++i) {
            if (authSet[i] == account) return true;
        }
        return false;
    }

    /// @inheritdoc ISubsidyAccounting
    function getNullifiers(address account) external view virtual onlyProxy returns (uint256[] memory) {
        return nullifiersOf[currentAction()][account];
    }

    /// @inheritdoc ISubsidyAccounting
    function isClaimed(uint256 nullifier, uint64 issuerSchemaId) external view virtual onlyProxy returns (bool) {
        return claimed[currentAction()][nullifier][issuerSchemaId];
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  ADMIN                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @inheritdoc ISubsidyAccounting
    /// @dev Bumps `_registrationVersion`, which structurally invalidates every in-flight
    ///      per-period record under the previous verifier — `currentAction()` changes value,
    ///      so prior-action slots are unreachable from any view or hot-path lookup.
    function setWorldIDVerifier(IWorldIDVerifier worldIDVerifier_) external virtual onlyProxy onlyOwner {
        if (address(worldIDVerifier_) == address(0)) revert AddressZero();
        worldIDVerifier = worldIDVerifier_;
        ++_registrationVersion;
        emit WorldIDVerifierSet(address(worldIDVerifier_));
    }

    /// @inheritdoc ISubsidyAccounting
    function setCredentialBudget(uint64 issuerSchemaId, uint256 budgetWei) external virtual onlyProxy onlyOwner {
        credentialBudget[issuerSchemaId] = budgetWei;
        emit CredentialBudgetSet(issuerSchemaId, budgetWei);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                INTERNAL                                 ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @dev Current subsidy period derived from `block.timestamp`.
    function currentPeriod() internal view returns (uint64) {
        return uint64(block.timestamp / PERIOD_LENGTH);
    }

    /// @dev Action key folding `period` and `_registrationVersion` — outer key for every
    ///      period-scoped storage map. Mirrored bit-for-bit by the revm debit hook, so the
    ///      derivation is part of the WIP-1002 protocol commitment and MUST NOT change.
    function _actionForPeriod(uint64 period) internal view returns (uint256) {
        return uint256(keccak256(abi.encodePacked("period_proof", period, _registrationVersion))) >> 8;
    }

    /// @dev Action for the current block timestamp.
    function currentAction() internal view returns (uint256) {
        return _actionForPeriod(currentPeriod());
    }

    /// @dev Validate every `items[i].issuerSchemaId`, mark each as claimed under
    ///      `(action, nullifier)`, and return the summed configured budget. Reverts on
    ///      duplicates or an already-claimed credential. Pulled out of `claimSubsidy` to
    ///      keep its frame within the 16-local stack limit.
    function _accrueClaimedCredentials(uint256 action, uint256 nullifier, ClaimItem[] calldata items)
        internal
        returns (uint256 totalBudget)
    {
        uint256 itemsLen = items.length;
        for (uint256 i = 0; i < itemsLen; ++i) {
            uint64 schemaId = items[i].issuerSchemaId;
            if (claimed[action][nullifier][schemaId]) revert DuplicateIssuerSchemaId(schemaId);
            claimed[action][nullifier][schemaId] = true;
            totalBudget += credentialBudget[schemaId];
        }
    }

    /// @dev Verify every `items[i].proof` as a Uniqueness Proof for the current `action`
    ///      against the shared `signalHash`. Reverts atomically if any item is rejected.
    ///      Verifier inputs `nonce`, `expiresAtMin`, `credentialGenesisIssuedAtMin` are
    ///      supplied deterministically from `PROOF_NONCE`, `period * PERIOD_LENGTH`, and
    ///      `CREDENTIAL_GENESIS_ISSUED_AT_MIN`.
    function _verifyClaimItems(
        uint256 nullifier,
        uint64 period,
        uint256 action,
        uint256 signalHash,
        ClaimItem[] calldata items
    ) internal view {
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
                items[i].issuerSchemaId,
                CREDENTIAL_GENESIS_ISSUED_AT_MIN,
                items[i].proof
            );
        }
    }

    /// @dev Full-replace the authorized set under `(action, nullifier)` with `newSet`.
    ///      Drops `nullifier` from every old address's reverse index via swap-pop, deletes
    ///      the old forward set, then appends `newSet` with the same checks as `_addAddresses`.
    function _replaceAuthorized(uint256 action, uint256 nullifier, address[] calldata newSet) internal {
        address[] storage authSet = authorized[action][nullifier];
        uint256 oldLen = authSet.length;
        for (uint256 i = 0; i < oldLen; ++i) {
            address oldAddr = authSet[i];
            uint256[] storage rev = nullifiersOf[action][oldAddr];
            uint256 rlen = rev.length;
            for (uint256 j = 0; j < rlen; ++j) {
                if (rev[j] == nullifier) {
                    rev[j] = rev[rlen - 1];
                    rev.pop();
                    break;
                }
            }
        }
        delete authorized[action][nullifier];
        _addAddresses(action, nullifier, newSet);
    }

    /// @dev Append `addrs` to the authorized set under `(action, nullifier)` and to each
    ///      address's reverse index. Reverts on zero address or any duplicate (against
    ///      either the existing set or earlier entries in the payload).
    function _addAddresses(uint256 action, uint256 nullifier, address[] calldata addrs) internal {
        address[] storage authSet = authorized[action][nullifier];
        uint256 len = addrs.length;
        for (uint256 i = 0; i < len; ++i) {
            address acct = addrs[i];
            if (acct == address(0)) revert AddressZero();
            uint256 alen = authSet.length;
            for (uint256 j = 0; j < alen; ++j) {
                if (authSet[j] == acct) revert DuplicateAuthorizedAddress(acct);
            }
            authSet.push(acct);
            nullifiersOf[action][acct].push(nullifier);
        }
    }
}
