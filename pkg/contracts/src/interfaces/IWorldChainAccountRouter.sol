// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {WorldChainAccountVerifier} from "./IWorldChainAccountManager.sol";
import {IWorldChainSessionVerifier} from "./IWorldChainSessionVerifier.sol";

/// @title IWorldChainAccountRouter
/// @author 0xOsiris, World Contributors
/// @notice Interface for the account router. The account router is the only validation target
///         called by the manager. It dispatches to configured verifier implementations using a
///         single controlled `DELEGATECALL`, so verifier implementations execute against the
///         account's storage.
/// @custom:security-contact security@toolsforhumanity.com
interface IWorldChainAccountRouter {
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
