// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IWorldIDVerifier} from "./IWorldIDVerifier.sol";

/// @title ISubsidyAccounting
/// @author Worldcoin
/// @notice Interface for the WIP-1002 World ID Subsidy Accounting contract.
///
///         Per-credential, ETH-denominated transaction-fee subsidies for verified World ID
///         holders on World Chain. Each `nullifier` keys a per-period subsidy record holding
///         the remaining budget, the `sessionId` for follow-up Session Proofs, the set of
///         claimed `issuerSchemaId` values, the authorized-address set, and a monotonic
///         update nonce.
///
///         Initial claims accept a multi-item Uniqueness-Proof bundle for the action
///         `keccak256("period_proof" || periodNumber)`; subsequent additions and address
///         updates use Session Proofs verified against the stored `sessionId`. Records
///         lazily expire when their `periodNumber` no longer matches the current period.
///
/// @custom:security-contact security@toolsforhumanity.com
interface ISubsidyAccounting {
    ///////////////////////////////////////////////////////////////////////////////
    ///                                  TYPES                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice One per-credential Uniqueness Proof emitted from a single multi-item
    ///         `ProofRequest`. All items in a `claimSubsidy` call MUST share the same
    ///         `nullifier` public output and the same recomputed `signalHash`.
    /// @param issuerSchemaId Credential schema/issuer identifier.
    /// @param proof Compressed Groth16 proof `[a, b0, b1, c, merkle_root]`.
    struct ClaimItem {
        uint64 issuerSchemaId;
        uint256[5] proof;
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                 EVENTS                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Emitted once when the implementation is initialized behind its proxy.
    event SubsidyAccountingImplInitialized(IWorldIDVerifier indexed worldIDVerifier, address indexed owner);

    /// @notice Emitted when the World ID verifier address is set or replaced.
    event WorldIDVerifierSet(address indexed worldIDVerifier);

    /// @notice Emitted when the per-credential budget is configured.
    event CredentialBudgetSet(uint64 indexed issuerSchemaId, uint256 budgetWei);

    /// @notice Emitted when a subsidy record is opened for the current period.
    event SubsidyClaimed(
        uint256 indexed nullifier, uint256 indexed sessionId, uint64 periodNumber, uint256 totalBudgetWei
    );

    /// @notice Emitted when an additional credential is claimed under an existing record.
    event AdditionalCredentialClaimed(uint256 indexed nullifier, uint64 indexed issuerSchemaId, uint256 budgetWei);

    /// @notice Emitted when the authorized-address set of a record is replaced. Carries the
    ///         post-update set in full; consumers SHOULD NOT diff against prior state.
    event AuthorizedSetUpdated(uint256 indexed nullifier, address[] newSet);

    ///////////////////////////////////////////////////////////////////////////////
    ///                                 ERRORS                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Thrown when a required address parameter is the zero address.
    error AddressZero();

    /// @notice Thrown by entry points whose body has not yet been ported in. Skeleton-entry
    ///         placeholder; replaced by real bodies in follow-up entries on the shared PR.
    error NotImplemented();

    /// @notice Thrown when `claimSubsidy` is called with an empty `items` array.
    error EmptyItems();

    /// @notice Thrown when `claimSubsidy` targets a `nullifier` that already has a record
    ///         in the current period.
    error RecordAlreadyExists();

    /// @notice Thrown when an entry point requiring an existing record finds none in the
    ///         current period.
    error RecordDoesNotExist();

    /// @notice Thrown when `claimSubsidy` is called with two items carrying the same
    ///         `issuerSchemaId`.
    error DuplicateIssuerSchemaId(uint64 issuerSchemaId);

    /// @notice Thrown when `claimAdditionalCredential` targets an `issuerSchemaId` already
    ///         claimed under the same `nullifier` this period.
    error CredentialAlreadyClaimed(uint64 issuerSchemaId);

    /// @notice Thrown when the supplied `nonce` does not match the record's `updateNonce`.
    error StaleUpdateNonce(uint64 supplied, uint64 expected);

    /// @notice Thrown when a `setAuthorized` payload contains a duplicate address (either of
    ///         an address already in the running set or a duplicate within the payload).
    error DuplicateAuthorizedAddress(address account);

    /// @notice Thrown when the summed per-credential budget for a `claimSubsidy` call
    ///         exceeds `type(uint128).max`, which is the on-chain `remainingWei` width.
    error BudgetOverflow(uint256 totalWei);

    ///////////////////////////////////////////////////////////////////////////////
    ///                              EXTERNAL API                               ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Atomic initial per-period claim. Verifies every Uniqueness Proof in `items`
    ///         against the recomputed `ClaimSubsidySignal` `signalHash`, opens a subsidy
    ///         record keyed by `nullifier`, stores `sessionId` and `addAddresses`, marks
    ///         every supplied `issuerSchemaId` as claimed, and credits the summed budget.
    /// @dev MUST revert if `items` is empty, if `nullifier` already has a record in the
    ///      current period, if any two items share an `issuerSchemaId`, or if any
    ///      `signalHash` mismatches.
    /// @param nullifier The per-period nullifier shared by every item.
    /// @param sessionId Session identifier installed at first-claim, used by Session Proofs.
    /// @param addAddresses Initial authorized-address set for the record.
    /// @param items Per-credential Uniqueness Proofs.
    function claimSubsidy(
        uint256 nullifier,
        uint256 sessionId,
        address[] calldata addAddresses,
        ClaimItem[] calldata items
    ) external;

    /// @notice Adds budget for a credential acquired after the initial claim. Verifies a
    ///         Session Proof against the stored `sessionId` for `nullifier`.
    /// @dev MUST revert if `nullifier` has no record in the current period or if
    ///      `issuerSchemaId` is already claimed. `sessionNullifier` and `sessionAction` are
    ///      the two per-proof public inputs `verifySession` consumes; both flow through
    ///      unmodified.
    function claimAdditionalCredential(
        uint256 nullifier,
        uint64 issuerSchemaId,
        uint256 sessionNullifier,
        uint256 sessionAction,
        uint256[5] calldata proof
    ) external;

    /// @notice Full-replaces the authorized-address set for an existing record. Idempotent at
    ///         the API level. Empty `newSet` revokes all addresses. Verifies a Session Proof
    ///         against the stored `sessionId` for `nullifier`.
    /// @dev `nonce` MUST equal the record's monotonic `updateNonce`; the record's nonce is
    ///      bumped on success, invalidating any earlier Session Proof for a set replacement.
    ///      `sessionNullifier` and `sessionAction` are the two per-proof public inputs
    ///      `verifySession` consumes; both flow through unmodified.
    function setAuthorized(
        uint256 nullifier,
        uint64 nonce,
        address[] calldata newSet,
        uint256 sessionNullifier,
        uint256 sessionAction,
        uint256[5] calldata proof
    ) external;

    /// @notice Get remaining subsidy budget (in Wei) for a record in the current period.
    /// @return remainingWei Returns 0 if the record is absent or expired.
    function getBudget(uint256 nullifier) external view returns (uint256 remainingWei);

    /// @notice Get the total remaining subsidy budget (in Wei) available to an address —
    ///         the SUM of `remainingWei` across every current-period record the account is
    ///         authorized under. Mirrors the pool that `consumeBudget(account, ...)` draws
    ///         from.
    function getBudget(address account) external view returns (uint256 remainingWei);

    /// @notice Whether `account` is authorized under the record keyed by `nullifier` in the
    ///         current period.
    function isAuthorized(address account, uint256 nullifier) external view returns (bool);

    /// @notice All current-period nullifiers under which `account` is authorized.
    function getNullifiers(address account) external view returns (uint256[] memory);

    /// @notice Whether `issuerSchemaId` has been claimed under `nullifier` in the current
    ///         period.
    function isClaimed(uint256 nullifier, uint64 issuerSchemaId) external view returns (bool);

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  ADMIN                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Set the claimable budget for a credential type. Owner-only.
    function setCredentialBudget(uint64 issuerSchemaId, uint256 budgetWei) external;

    /// @notice Replace the World ID verifier. Owner-only. MUST revert on zero address.
    function setWorldIDVerifier(IWorldIDVerifier worldIDVerifier) external;
}
