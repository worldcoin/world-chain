// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IWorldChainAccountManager} from "./interfaces/IWorldChainAccountManager.sol";
import {IWorldChainAccountRouter} from "./interfaces/IWorldChainAccountRouter.sol";
import {IWorldChainSessionVerifier} from "./interfaces/IWorldChainSessionVerifier.sol";
import {IWorldChainAccountHooks} from "./interfaces/IWorldChainAccountHooks.sol";
import {VerifierDispatch} from "./libraries/VerifierDispatch.sol";
import {WorldChainAccountConstants} from "./libraries/WorldChainAccountConstants.sol";
import {AdminAlreadyInstalled, AdminNotInstalled, ManagerOnly} from "./libraries/WorldChainAccountErrors.sol";

/// @title WorldChainAccountRouter
/// @author Worldcoin
/// @notice Per-account dispatcher that owns the verifier-installation lifecycle and delegates
///         admin and session validation calls into installed EIP-1271 verifier implementations.
/// @dev A router instance lives at each WIP-1001 account address (or, in the Solidity
///      reference implementation, at the `CREATE2` address tracked by the manager). Verifier
///      implementations execute against this router's storage via `DELEGATECALL`, which under
///      the manager's outer `STATICCALL` is non-state-mutating end-to-end.
///
///      The router is intentionally minimal and non-upgradeable:
///        - `manager` and `validationGasLimit` are set once in the constructor and never
///          mutated thereafter. They live in storage rather than as `immutable` values so the
///          deployed runtime code is byte-identical across all router instances; this is
///          what makes `WORLD_CHAIN_ACCOUNT_ROUTER_CODE_HASH` a single fixed value across
///          accounts.
///        - The admin verifier is installed exactly once.
///        - The session-verifier set is fully replaced atomically via {installKeyRing}.
///        - No path other than {installAdmin}/{installKeyRing}, gated on `manager`, can mutate
///          dispatch state. Verifier-owned storage namespaces are governed by each verifier
///          implementation.
/// @custom:security-contact security@toolsforhumanity.com
contract WorldChainAccountRouter is IWorldChainAccountRouter {
    using VerifierDispatch for address;

    ///////////////////////////////////////////////////////////////////////////////
    ///                                 STORAGE                                 ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @inheritdoc IWorldChainAccountRouter
    /// @dev Slot 0. Set once in the constructor.
    address public override manager;

    /// @notice Gas forwarded to verifier `DELEGATECALL`s inside the validation dispatch
    ///         functions. Set once in the constructor.
    /// @dev Packed with `manager` into slot 0 (20 + 8 = 28 bytes).
    uint64 public validationGasLimit;

    /// @inheritdoc IWorldChainAccountRouter
    /// @dev Slot 1.
    address public override adminVerifier;

    /// @notice Ordered list of currently installed session-verifier addresses.
    /// @dev Iteration is only used at {installKeyRing} time to clear the old index map.
    address[] private _sessionVerifierList;

    /// @notice 1-indexed position of each session verifier in `_sessionVerifierList`. Zero
    ///         means "not installed".
    mapping(address verifier => uint256 indexPlusOne) private _sessionVerifierIndexPlusOne;

    ///////////////////////////////////////////////////////////////////////////////
    ///                              CONSTRUCTION                               ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @param manager_ The manager address authorised to mutate this router.
    /// @param validationGasLimit_ The EIP-1271 validation gas limit at deployment time.
    constructor(address manager_, uint64 validationGasLimit_) {
        manager = manager_;
        validationGasLimit = validationGasLimit_;
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              INSTALL HOOKS                              ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @inheritdoc IWorldChainAccountRouter
    /// @dev Single-shot: reverts if an admin verifier is already installed. The manager calls
    ///      this exactly once per account inside `create`, before installing the key ring.
    function installAdmin(IWorldChainAccountManager.WorldChainAccountVerifier calldata admin) external override {
        if (msg.sender != manager) revert ManagerOnly();
        if (adminVerifier != address(0)) revert AdminAlreadyInstalled();

        adminVerifier = admin.verifier;

        // DELEGATECALL into the admin verifier's install hook so any scoped state is
        // materialised into this router's storage namespace.
        _delegateInstall(admin.verifier, admin.installation);
    }

    /// @inheritdoc IWorldChainAccountRouter
    function installKeyRing(IWorldChainAccountManager.WorldChainAccountVerifier[] calldata sessionVerifiers)
        external
        override
    {
        if (msg.sender != manager) revert ManagerOnly();
        if (adminVerifier == address(0)) revert AdminNotInstalled();

        // Clear the old index map by iterating the existing list (cheaper than enumerating
        // mapping keys and bounded by `MAX_SESSION_VERIFIERS`).
        address[] storage list = _sessionVerifierList;
        uint256 oldLen = list.length;
        for (uint256 i; i < oldLen; ++i) {
            delete _sessionVerifierIndexPlusOne[list[i]];
        }
        // Truncate the list. Iterating + delete on the array entries themselves is unnecessary
        // because we overwrite or shrink below.
        assembly ("memory-safe") {
            sstore(list.slot, 0)
        }

        // Install the new set in order and DELEGATECALL each verifier's `install` hook.
        uint256 newLen = sessionVerifiers.length;
        for (uint256 i; i < newLen; ++i) {
            address verifier = sessionVerifiers[i].verifier;
            list.push(verifier);
            _sessionVerifierIndexPlusOne[verifier] = i + 1;
            _delegateInstall(verifier, sessionVerifiers[i].installation);
        }
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                            VALIDATION DISPATCH                          ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @inheritdoc IWorldChainAccountRouter
    function isValidSignatureForAdmin(bytes32 hash, bytes calldata signature)
        external
        view
        override
        returns (bytes4 magicValue)
    {
        address verifier = adminVerifier;
        if (verifier == address(0)) {
            return WorldChainAccountConstants.EIP1271_FAILURE_VALUE;
        }
        return
            verifier.delegatecallEip1271(validationGasLimit, VerifierDispatch.encodeIsValidSignature(hash, signature));
    }

    /// @inheritdoc IWorldChainAccountRouter
    function isValidSignatureForVerifier(address verifier, bytes32 hash, bytes calldata signature)
        external
        view
        override
        returns (bytes4 magicValue)
    {
        if (_sessionVerifierIndexPlusOne[verifier] == 0) {
            // Spec: unknown session verifier addresses MUST fail without executing verifier
            //       implementation code.
            return WorldChainAccountConstants.EIP1271_FAILURE_VALUE;
        }
        return
            verifier.delegatecallEip1271(validationGasLimit, VerifierDispatch.encodeIsValidSignature(hash, signature));
    }

    /// @inheritdoc IWorldChainAccountRouter
    function evaluateSessionPolicyForVerifier(
        address verifier,
        IWorldChainSessionVerifier.ExecutionTraceContext calldata context
    ) external view override returns (bool allowed) {
        if (_sessionVerifierIndexPlusOne[verifier] == 0) {
            return false;
        }
        bytes memory callData =
            abi.encodeWithSelector(IWorldChainSessionVerifier.evaluateSessionPolicy.selector, context);
        return verifier.delegatecallPolicy(validationGasLimit, callData);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  VIEWS                                  ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Returns the ordered list of currently installed session-verifier addresses.
    /// @dev Useful for off-chain dispatch and conformance tests. The manager also tracks this
    ///      set authoritatively for view queries.
    function installedSessionVerifiers() external view returns (address[] memory) {
        return _sessionVerifierList;
    }

    /// @notice Returns whether `verifier` is currently installed as a session verifier.
    function isInstalledSessionVerifier(address verifier) external view returns (bool) {
        return _sessionVerifierIndexPlusOne[verifier] != 0;
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                INTERNAL                                 ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @dev DELEGATECALLs `verifier.install(installation)` and bubbles any revert payload from
    ///      the verifier so installation errors are surfaceable to integrators. Reverts with
    ///      no data when the call fails with no return data.
    function _delegateInstall(address verifier, bytes calldata installation) private {
        bytes memory callData = abi.encodeWithSelector(IWorldChainAccountHooks.install.selector, installation);
        assembly ("memory-safe") {
            let success := delegatecall(gas(), verifier, add(callData, 0x20), mload(callData), 0x00, 0x00)
            if iszero(success) {
                let size := returndatasize()
                returndatacopy(0x00, 0x00, size)
                revert(0x00, size)
            }
        }
    }
}
