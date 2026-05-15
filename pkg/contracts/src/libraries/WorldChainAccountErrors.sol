// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @title WorldChainAccountErrors
/// @author Worldcoin
/// @notice Custom errors shared by the WIP-1001 manager, router, and supporting libraries.
/// @dev Defined as free-floating errors so both the manager and router can `revert` with the
///      same selectors regardless of inheritance. Including this file is sufficient; no
///      contract inherits from it.
/// @custom:security-contact security@toolsforhumanity.com

/// @notice Thrown when a caller other than the configured manager invokes a manager-only
///         function on the router.
error ManagerOnly();

/// @notice Thrown when an address parameter that must be non-zero is zero.
error AddressZero();

/// @notice Thrown when `create` is invoked for an account address that already exists.
error AccountAlreadyExists(address account);

/// @notice Thrown when a mutating or view call targets an account that does not exist.
error AccountDoesNotExist(address account);

/// @notice Thrown when `create` is invoked with `admin.verifier == address(0)`.
error AdminVerifierZero();

/// @notice Thrown when an admin or session verifier address has no deployed code.
error VerifierUndeployed(address verifier);

/// @notice Thrown when a session-verifier entry has `verifier == address(0)`.
error SessionVerifierZero();

/// @notice Thrown when the session-verifier list contains the same `verifier` more than once.
error DuplicateSessionVerifier(address verifier);

/// @notice Thrown when the session-verifier list is empty.
error EmptySessionVerifierSet();

/// @notice Thrown when the session-verifier list exceeds `MAX_SESSION_VERIFIERS`.
error TooManySessionVerifiers(uint256 actual, uint256 max);

/// @notice Thrown when a verifier `installation` exceeds the active byte cap.
error InstallationTooLarge(uint256 actual, uint256 max);

/// @notice Thrown when `adminAuthorization` exceeds the active byte cap.
error AdminAuthorizationTooLarge(uint256 actual, uint256 max);

/// @notice Thrown when `setKeyRing` is invoked with the same key-ring hash as the current one.
error KeyRingUnchanged(bytes32 keyRingHash);

/// @notice Thrown when `setKeyRing`'s `expectedCurrentKeyRingHash` does not match the
///         account's current `keyRingHash`.
error StaleKeyRingHash(bytes32 expected, bytes32 actual);

/// @notice Thrown when the admin verifier rejects, reverts, or returns malformed data inside
///         the restricted validation frame.
error AdminAuthorizationFailed();

/// @notice Thrown by router views when `verifier` is not a member of the active
///         session-verifier set. Validation paths do not raise this — they return a non-magic
///         value or `false` — but explicit getters surface it.
error UnauthorizedSessionVerifier(address verifier);

/// @notice Thrown when an attempt is made to mutate activation parameters after they have
///         been frozen.
error ActivationParametersAlreadyFrozen();

/// @notice Thrown when `installAdmin` is called after the admin verifier has already been set.
error AdminAlreadyInstalled();

/// @notice Thrown when `installKeyRing` is called from the manager before the router has been
///         initialised with an admin verifier.
error AdminNotInstalled();
