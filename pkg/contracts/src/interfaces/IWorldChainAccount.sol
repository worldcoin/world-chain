// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {WorldChainAccountVerifier} from "./IWorldChainAccountManager.sol";
import {IWorldChainSessionVerifier} from "./IWorldChainSessionVerifier.sol";

/// @title IWorldChainAccount
/// @author 0xOsiris, World Contributors
/// @notice Public interface of the WIP-1001 account router contract deployed (via beacon proxy)
///         at every World Chain account address. The router is the only validation target called
///         by `WORLD_CHAIN_ACCOUNT_MANAGER`. It dispatches to configured verifier implementations
///         using a single controlled `DELEGATECALL`, so verifier implementations execute against
///         the account's storage.
/// @custom:security-contact security@toolsforhumanity.com
interface IWorldChainAccount {
    // ─── Storage layout ───────────────────────────────────────────────────────

    /// @custom:storage-location erc7201:worldchain.account.keyring
    /// @dev EIP-7201 style storage for the account router. DO __NOT__ REORDER THIS STRUCT.
    /// @param adminVerifier The immutable admin verifier implementation address. Set by the
    ///                      single permitted `installAdmin` invocation and never overwritten.
    /// @param keyRingHash The active key ring's canonical hash,
    ///                    `keccak256(abi.encode(sessionVerifiers))`. Matches the WIP-1001
    ///                    manager-side `keyRingHash` and serves as the per-account generation
    ///                    marker for session-verifier membership.
    /// @param sessionKeyRing Per-verifier installation marker — the `keyRingHash` at the time the
    ///                      verifier was installed. A verifier `v` is a member of the active key
    ///                      ring iff `sessionKeyRing[v] == keyRingHash != 0`.
    struct KeyRingStorage {
        address adminVerifier;
        bytes32 keyRingHash;
        mapping(address verifier => bytes32 keyRingHash) sessionKeyRing;
    }

    // ─── Versioning ───────────────────────────────────────────────────────────

    /// @notice The contract version.
    /// @custom:semver v1.0.0
    function VERSION() external view returns (uint8);

    // ─── View-only state (router storage) ─────────────────────────────────────

    /// @notice The `WORLD_CHAIN_ACCOUNT_MANAGER` predeploy address authorized to call every
    ///         state-mutating entry point on this account.
    // solhint-disable-next-line func-name-mixedcase
    function MANAGER() external view returns (address);

    /// @notice Returns the installed admin verifier address, or `address(0)` if `installAdmin` has
    ///         not been called.
    // solhint-disable-next-line func-name-mixedcase
    function ADMIN_VERIFIER() external view returns (address);

    /// @notice Returns the canonical `keyRingHash` of the active session verifier set,
    ///         `keccak256(abi.encode(sessionVerifiers))`. A zero hash means no key ring has been
    ///         installed.
    // solhint-disable-next-line func-name-mixedcase
    function KEYRING_HASH() external view returns (bytes32);

    /// @notice Per-verifier installation marker. Returns the `keyRingHash` recorded at the time
    ///         `verifier` was installed. The verifier is a member of the active key ring iff this
    ///         value equals the current `KEYRING_HASH()` and is non-zero.
    /// @param verifier The verifier address to query.
    function sessionKeyRing(address verifier) external view returns (bytes32);

    // ─── State-mutating entry points ──────────────────────────────────────────

    /// @notice Installs the immutable admin verifier into the account router. MUST reject any
    ///         caller other than `WORLD_CHAIN_ACCOUNT_MANAGER`.
    /// @param admin The admin verifier descriptor to install.
    function installAdmin(WorldChainAccountVerifier calldata admin) external;

    /// @notice Installs a session verifier key ring into the account router. MUST reject any
    ///         caller other than `WORLD_CHAIN_ACCOUNT_MANAGER`. Replaces any previously installed
    ///         key ring on `setKeyRing`.
    /// @param sessionVerifiers The ordered session verifier set to install.
    function installKeyRing(WorldChainAccountVerifier[] calldata sessionVerifiers) external;

    /// @notice EIP-1271 admin signature dispatch. Dispatches to the installed `admin.verifier`
    ///         using a single controlled `DELEGATECALL` with calldata
    ///         `IERC1271.isValidSignature.selector || abi.encode(hash, signature)`. MUST be
    ///         callable by `WORLD_CHAIN_ACCOUNT_MANAGER` in a restricted validation frame.
    /// @dev    Not declared `view` because Solidity treats inline-assembly `DELEGATECALL` as
    ///         potentially state-modifying. Read-only semantics are enforced by the manager
    ///         issuing the outer call via `STATICCALL`, per WIP-1001.
    /// @param hash The domain-separated admin operation hash being authorized.
    /// @param signature The admin authorization payload.
    /// @return magicValue `EIP1271_MAGIC_VALUE` when the admin verifier accepts the signature.
    function isValidSignatureForAdmin(bytes32 hash, bytes calldata signature) external returns (bytes4 magicValue);

    /// @notice EIP-1271 session signature dispatch. Dispatches to an installed session verifier
    ///         using a single controlled `DELEGATECALL` with calldata
    ///         `IERC1271.isValidSignature.selector || abi.encode(hash, signature)`. Unknown
    ///         session verifier addresses MUST fail without executing verifier implementation
    ///         code. MUST be callable by `WORLD_CHAIN_ACCOUNT_MANAGER` in a restricted validation
    ///         frame.
    /// @dev    Not declared `view` because Solidity treats inline-assembly `DELEGATECALL` as
    ///         potentially state-modifying. Read-only semantics are enforced by the manager
    ///         issuing the outer call via `STATICCALL`, per WIP-1001.
    /// @param verifier The session verifier address; MUST be installed in the current key ring.
    /// @param hash The signing hash being authorized.
    /// @param signature The session signature payload.
    /// @return magicValue `EIP1271_MAGIC_VALUE` when the session verifier accepts the signature.
    function isValidSignatureForVerifier(address verifier, bytes32 hash, bytes calldata signature)
        external
        returns (bytes4 magicValue);

    /// @notice Session-policy dispatch. Dispatches to an installed session verifier using a single
    ///         controlled `DELEGATECALL` with calldata
    ///         `IWorldChainSessionVerifier.evaluateSessionPolicy.selector || abi.encode(context)`.
    ///         Unknown session verifier addresses MUST fail without executing verifier
    ///         implementation code. MUST be callable by `WORLD_CHAIN_ACCOUNT_MANAGER` in a
    ///         restricted validation frame.
    /// @dev    Not declared `view` because Solidity treats inline-assembly `DELEGATECALL` as
    ///         potentially state-modifying. Read-only semantics are enforced by the manager
    ///         issuing the outer call via `STATICCALL`, per WIP-1001.
    /// @param verifier The session verifier address; MUST be installed in the current key ring.
    /// @param context The canonical transaction context paired with the tentative execution trace.
    /// @return allowed `true` iff the session verifier permits the transaction.
    function evaluateSessionPolicyForVerifier(
        address verifier,
        IWorldChainSessionVerifier.ExecutionTraceContext calldata context
    ) external returns (bool allowed);
}
