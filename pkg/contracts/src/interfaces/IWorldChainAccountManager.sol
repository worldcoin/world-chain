// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @notice Verifier descriptor used by both the admin slot and each entry in a session key ring.
/// @param verifier Address of an EIP-1271 verifier implementation.
/// @param installation Verifier-defined installation payload. Opaque to the protocol; committed
///                     to by `adminHash`, `createHash`, and `keyRingHash`. Length MUST NOT exceed
///                     `MAX_VERIFIER_INSTALL_DATA_BYTES`.
struct WorldChainAccountVerifier {
    address verifier;
    bytes installation;
}

/// @title IWorldChainAccountManager
/// @author 0xOsiris, World Contributors
/// @notice Interface for the `WORLD_CHAIN_ACCOUNT_MANAGER` predeploy. World Chain accounts are
///         created and managed by `WORLD_CHAIN_ACCOUNT_MANAGER`.
/// @custom:security-contact security@toolsforhumanity.com
interface IWorldChainAccountManager {
    /// @notice Creates a new World Chain account. Derives the account address as a hash of
    ///         `WORLD_CHAIN_ACCOUNT_DOMAIN`, `chainId`, `WORLD_CHAIN_ACCOUNT_MANAGER`,
    ///         `WORLD_CHAIN_ACCOUNT_ROUTER_CODE_HASH`, `adminHash`, and `accountSalt`. Atomically
    ///         installs the canonical account-router runtime bytecode at the derived address,
    ///         installs the admin and initial key ring through the account router, and verifies
    ///         the `createHash` against the account router's admin validation path.
    /// @param admin The immutable admin verifier descriptor bound into the account address.
    /// @param accountSalt User-chosen salt folded into the account address derivation.
    /// @param initialSessionVerifiers The initial session verifier set; length MUST be in
    ///                                `[1, MAX_SESSION_VERIFIERS]` and MUST contain no duplicate
    ///                                or zero verifier addresses.
    /// @param adminAuthorization Admin authorization payload validated against `createHash`. Length
    ///                           MUST NOT exceed `MAX_ADMIN_AUTHORIZATION_BYTES`.
    /// @return account The deterministic address of the newly created account.
    function create(
        WorldChainAccountVerifier calldata admin,
        bytes32 accountSalt,
        WorldChainAccountVerifier[] calldata initialSessionVerifiers,
        bytes calldata adminAuthorization
    ) external returns (address account);

    /// @notice The only post-creation method that mutates the key ring. Replaces the account's
    ///         full `sessionVerifiers` set with the supplied set, increments `adminNonce`, and
    ///         updates `keyRingHash` to `keccak256(abi.encode(sessionVerifiers))`. Verifies the
    ///         `setKeyRingHash` against the account router's admin validation path.
    /// @param account The account whose key ring is being replaced. MUST exist.
    /// @param expectedCurrentKeyRingHash The account's current `keyRingHash`; rejected if stale.
    /// @param sessionVerifiers The replacement session verifier set; length MUST be in
    ///                         `[1, MAX_SESSION_VERIFIERS]` and MUST contain no duplicate, zero,
    ///                         undeployed, or oversized entries. The resulting `keyRingHash` MUST
    ///                         differ from the current `keyRingHash`.
    /// @param adminAuthorization Admin authorization payload validated against `setKeyRingHash`.
    ///                           Length MUST NOT exceed `MAX_ADMIN_AUTHORIZATION_BYTES`.
    function setKeyRing(
        address account,
        bytes32 expectedCurrentKeyRingHash,
        WorldChainAccountVerifier[] calldata sessionVerifiers,
        bytes calldata adminAuthorization
    ) external;

    /// @notice Returns the immutable admin verifier descriptor for `account`. The full `admin`
    ///         value is immutable for the life of the account address.
    function getAdmin(address account) external view returns (WorldChainAccountVerifier memory);

    /// @notice Returns the account's `adminNonce`. Consumed only by admin operations.
    function getAdminNonce(address account) external view returns (uint64);

    /// @notice Returns the account's `transactionNonce`. Consumed only by successful `0x1D`
    ///         transaction execution attempts.
    function getTransactionNonce(address account) external view returns (uint64);

    /// @notice Returns `keccak256(abi.encode(sessionVerifiers))` for the account's current session
    ///         verifier set. Deterministic anchor used by clients to bind cached signature
    ///         validation; clients MUST discard cached results when this hash changes.
    function getKeyRingHash(address account) external view returns (bytes32);

    /// @notice Returns the account's current ordered session verifier set.
    function getSessionVerifiers(address account) external view returns (WorldChainAccountVerifier[] memory);

    /// @notice Returns whether `sessionVerifier` is a member of the account's current session
    ///         verifier set.
    function isAuthorizedSessionVerifier(address account, address sessionVerifier) external view returns (bool);

    /// @notice Returns the descriptor for `sessionVerifier` in the account's current session
    ///         verifier set.
    function getAuthorizedSessionVerifier(address account, address sessionVerifier)
        external
        view
        returns (WorldChainAccountVerifier memory);
}
