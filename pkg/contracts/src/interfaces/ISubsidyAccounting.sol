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
/// @dev Public-ABI deviation from WIP-1002 spec § "Subsidy Accounting Interface": session-
///      proof and Uniqueness-proof inputs are surfaced as `uint256[2] sessionNullifier`
///      and `uint256[5] proof` to match `IWorldIDVerifier`. The verifier-required scalar
///      inputs (`proofNonce`, `expiresAtMin`, `credentialGenesisIssuedAtMin`) are part of
///      every entry-point signature; the spec's compact `bytes proof` form is purely
///      illustrative. Consistent with the WIP-1001 `WorldIDAccountManager` stand-in.
///
/// @custom:security-contact security@toolsforhumanity.com
interface ISubsidyAccounting {
    ///////////////////////////////////////////////////////////////////////////////
    ///                                  TYPES                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice One per-credential Uniqueness Proof emitted from a single multi-item
    ///         `ProofRequest`. All items in a `claimSubsidy` call MUST share the same
    ///         `nullifier` public output and the same recomputed `signalHash`.
    /// @param issuerSchemaId Credential schema/issuer identifier. `uint256` for spec-fidelity;
    ///        bounds-checked against `type(uint64).max` before downcast for verifier dispatch.
    /// @param proofNonce Verifier request nonce for this proof.
    /// @param expiresAtMin Minimum credential expiration the proof asserts.
    /// @param credentialGenesisIssuedAtMin Minimum credential `genesis_issued_at`.
    /// @param proof Compressed Groth16 proof `[a, b0, b1, c, merkle_root]`.
    struct ClaimItem {
        uint256 issuerSchemaId;
        uint256 proofNonce;
        uint64 expiresAtMin;
        uint256 credentialGenesisIssuedAtMin;
        uint256[5] proof;
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                 EVENTS                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Emitted once when the implementation is initialized behind its proxy.
    event SubsidyAccountingImplInitialized(IWorldIDVerifier indexed worldIDVerifier, address indexed owner);

    /// @notice Emitted when the World ID verifier address is set or replaced.
    event WorldIDVerifierSet(address indexed worldIDVerifier);

    /// @notice Emitted when the budget-consumer address is set or replaced.
    /// @dev Zero address is a valid value — used to deprovision the consumer role.
    event BudgetConsumerSet(address indexed budgetConsumer);

    /// @notice Emitted when the per-credential budget is configured.
    event CredentialBudgetSet(uint64 indexed issuerSchemaId, uint256 budgetWei);

    /// @notice Emitted when a subsidy record is opened for the current period.
    event SubsidyClaimed(
        uint256 indexed nullifier, uint256 indexed sessionId, uint64 periodNumber, uint256 totalBudgetWei
    );

    /// @notice Emitted when an additional credential is claimed under an existing record.
    event AdditionalCredentialClaimed(uint256 indexed nullifier, uint64 indexed issuerSchemaId, uint256 budgetWei);

    /// @notice Emitted when the authorized-address set of a record is mutated.
    event AddressesUpdated(
        uint256 indexed nullifier, uint64 newUpdateNonce, address[] addedAddresses, address[] removedAddresses
    );

    /// @notice Emitted when budget is consumed against a record.
    event BudgetConsumed(uint256 indexed nullifier, uint256 amountWei, uint256 remainingWei);

    ///////////////////////////////////////////////////////////////////////////////
    ///                                 ERRORS                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Thrown when a required address parameter is the zero address.
    error AddressZero();

    /// @notice Thrown when an `issuerSchemaId` exceeds `type(uint64).max`.
    /// @dev The verifier interface and storage use `uint64`; oversized values are rejected
    ///      at the public boundary rather than silently truncated.
    error IssuerSchemaIdOverflow(uint256 value);

    /// @notice Thrown when an `updateAddresses` `nonce` exceeds `type(uint64).max`.
    error UpdateNonceOverflow(uint256 value);

    /// @notice Thrown when an entry point that has not yet been implemented is called.
    /// @dev Skeleton-PR placeholder — replaced by real method bodies in follow-up PRs.
    error NotImplemented();

    /// @notice Thrown when `consumeBudget` is called by an address other than `budgetConsumer`.
    error NotBudgetConsumer();

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
    ///      `issuerSchemaId` is already claimed.
    function claimAdditionalCredential(
        uint256 nullifier,
        uint256 issuerSchemaId,
        uint256 proofNonce,
        uint64 expiresAtMin,
        uint256 credentialGenesisIssuedAtMin,
        uint256[2] calldata sessionNullifier,
        uint256[5] calldata proof
    ) external;

    /// @notice Mutates the authorized-address set for an existing record. Verifies a
    ///         Session Proof against the stored `sessionId` for `nullifier`.
    /// @dev `nonce` MUST equal the record's monotonic `updateNonce`; the record's nonce is
    ///      bumped on success, invalidating any earlier Session Proof for an address update.
    function updateAddresses(
        uint256 nullifier,
        uint256 nonce,
        address[] calldata addAddresses,
        address[] calldata removeAddresses,
        uint256 proofNonce,
        uint64 expiresAtMin,
        uint256 credentialGenesisIssuedAtMin,
        uint256[2] calldata sessionNullifier,
        uint256[5] calldata proof
    ) external;

    /// @notice Consume budget for a record at transaction-execution time.
    /// @dev Restricted to `budgetConsumer`. Decrement `remainingWei` by `gasUsed * baseFee`,
    ///      saturating at 0.
    function consumeBudget(uint256 nullifier, uint256 gasUsed, uint256 baseFee) external;

    /// @notice Get remaining subsidy budget (in Wei) for a record in the current period.
    /// @return remainingWei Returns 0 if the record is absent or expired.
    function getBudget(uint256 nullifier) external view returns (uint256 remainingWei);

    /// @notice Get remaining subsidy budget (in Wei) available to an address in the current
    ///         period. Resolved via the same deterministic nullifier-selection rule as
    ///         `consumeBudget`.
    function getBudget(address account) external view returns (uint256 remainingWei);

    /// @notice Whether `account` is authorized under the record keyed by `nullifier` in the
    ///         current period.
    function isAuthorized(address account, uint256 nullifier) external view returns (bool);

    /// @notice All current-period nullifiers under which `account` is authorized.
    function getNullifiers(address account) external view returns (uint256[] memory);

    /// @notice Whether `issuerSchemaId` has been claimed under `nullifier` in the current
    ///         period.
    function isClaimed(uint256 nullifier, uint256 issuerSchemaId) external view returns (bool);

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  ADMIN                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Set the claimable budget for a credential type. Owner-only.
    function setCredentialBudget(uint256 issuerSchemaId, uint256 budgetWei) external;

    /// @notice Set the address authorized to call `consumeBudget`. Owner-only.
    /// @dev Zero address is valid — disables consumption until set again.
    function setBudgetConsumer(address budgetConsumer) external;

    /// @notice Replace the World ID verifier. Owner-only. MUST revert on zero address.
    function setWorldIDVerifier(IWorldIDVerifier worldIDVerifier) external;
}
