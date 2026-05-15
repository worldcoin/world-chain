// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @title IWorldChainAccountManager
/// @author Worldcoin
/// @notice Interface for the WIP-1001 World Chain Account Manager predeploy.
///
///         The manager owns the address-derivation, admin-signer binding, and key-ring lifecycle
///         for every World Chain account on the chain. Authorization is delegated entirely to
///         EIP-1271 admin/session verifier contracts dispatched through a per-account router.
/// @custom:security-contact security@toolsforhumanity.com
interface IWorldChainAccountManager {
    ///////////////////////////////////////////////////////////////////////////////
    ///                                  TYPES                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice An EIP-1271 verifier delegated into a World Chain account.
    /// @param verifier The deployed verifier implementation address.
    /// @param installation Opaque verifier-specific installation calldata. Parsed only by the
    ///        verifier implementation; the manager commits to it through `adminHash` and
    ///        `keyRingHash`.
    struct WorldChainAccountVerifier {
        address verifier;
        bytes installation;
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                 EVENTS                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Emitted once when the implementation is initialised behind its proxy.
    /// @param owner The Ownable2Step owner of the manager.
    /// @param eip1271ValidationGasLimit The activation parameter for EIP-1271 validation gas.
    /// @param maxVerifierInstallDataBytes Cap on each verifier `installation`.
    /// @param maxAdminAuthorizationBytes Cap on `adminAuthorization` calldata.
    event WorldChainAccountManagerInitialized(
        address indexed owner,
        uint64 eip1271ValidationGasLimit,
        uint32 maxVerifierInstallDataBytes,
        uint32 maxAdminAuthorizationBytes
    );

    /// @notice Emitted when an activation parameter is updated by the owner.
    event ActivationParameterUpdated(bytes32 indexed key, uint256 oldValue, uint256 newValue);

    /// @notice Emitted once when activation parameters are permanently frozen.
    event ActivationParametersFrozen();

    /// @notice Emitted on a successful `create`.
    /// @param account The deterministic account address.
    /// @param adminVerifier The admin verifier implementation address (`admin.verifier`).
    /// @param adminHash The hash of the full admin verifier configuration committed at creation.
    /// @param accountSalt The salt supplied at creation.
    /// @param keyRingHash The initial key-ring hash.
    event AccountCreated(
        address indexed account,
        address indexed adminVerifier,
        bytes32 indexed adminHash,
        bytes32 accountSalt,
        bytes32 keyRingHash
    );

    /// @notice Emitted on a successful `setKeyRing`.
    /// @param account The account whose key ring was replaced.
    /// @param previousKeyRingHash The key-ring hash before replacement.
    /// @param newKeyRingHash The key-ring hash after replacement.
    /// @param adminNonce The post-increment admin nonce.
    event KeyRingSet(
        address indexed account, bytes32 indexed previousKeyRingHash, bytes32 indexed newKeyRingHash, uint64 adminNonce
    );

    ///////////////////////////////////////////////////////////////////////////////
    ///                              EXTERNAL API                               ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Creates a new World Chain account.
    /// @param admin The immutable admin verifier configuration.
    /// @param accountSalt Caller-chosen salt folded into the account address.
    /// @param initialSessionVerifiers The initial set of session verifiers.
    ///        MUST be non-empty, at most `MAX_SESSION_VERIFIERS`, free of duplicates / zero
    ///        addresses, and each entry MUST be deployed and have an `installation` no longer
    ///        than `MAX_VERIFIER_INSTALL_DATA_BYTES`.
    /// @param adminAuthorization EIP-1271 authorization over the `createHash` produced by the
    ///        admin verifier. Bounded by `MAX_ADMIN_AUTHORIZATION_BYTES`.
    /// @return account The deterministic account address.
    function create(
        WorldChainAccountVerifier calldata admin,
        bytes32 accountSalt,
        WorldChainAccountVerifier[] calldata initialSessionVerifiers,
        bytes calldata adminAuthorization
    ) external returns (address account);

    /// @notice Replaces the full session-verifier set of `account`.
    /// @param account The target account.
    /// @param expectedCurrentKeyRingHash The caller-asserted current key-ring hash. Used to
    ///        prevent stale-set replacements.
    /// @param sessionVerifiers The new full session-verifier set.
    /// @param adminAuthorization EIP-1271 authorization over the `setKeyRingHash` produced by
    ///        the admin verifier with the current `adminNonce`.
    function setKeyRing(
        address account,
        bytes32 expectedCurrentKeyRingHash,
        WorldChainAccountVerifier[] calldata sessionVerifiers,
        bytes calldata adminAuthorization
    ) external;

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  VIEWS                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Returns the immutable admin verifier configuration for `account`.
    function getAdmin(address account) external view returns (WorldChainAccountVerifier memory);

    /// @notice Returns the current admin nonce of `account`.
    function getAdminNonce(address account) external view returns (uint64);

    /// @notice Returns the current `0x1D` transaction nonce of `account`.
    function getTransactionNonce(address account) external view returns (uint64);

    /// @notice Returns the current `keyRingHash` of `account`.
    function getKeyRingHash(address account) external view returns (bytes32);

    /// @notice Returns the current ordered session-verifier set of `account`.
    function getSessionVerifiers(address account) external view returns (WorldChainAccountVerifier[] memory);

    /// @notice Returns whether `sessionVerifier` is currently authorised for `account`.
    function isAuthorizedSessionVerifier(address account, address sessionVerifier) external view returns (bool);

    /// @notice Returns the installed configuration of `sessionVerifier` for `account`. Reverts
    ///         when `sessionVerifier` is not authorised.
    function getAuthorizedSessionVerifier(address account, address sessionVerifier)
        external
        view
        returns (WorldChainAccountVerifier memory);

    ///////////////////////////////////////////////////////////////////////////////
    ///                          REFERENCE-IMPL VIEWS                           ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Returns the runtime code hash of the account router used by this manager
    ///         instance. Folded into account-address derivation and admin-operation hashes.
    function getAccountRouterCodeHash() external view returns (bytes32);

    /// @notice Returns the deployed account-router address for `account` in the Solidity
    ///         reference implementation. In production the predeploy installs the router
    ///         natively at the spec-derived account address and this is the identity.
    function getAccountRouter(address account) external view returns (address);
}
