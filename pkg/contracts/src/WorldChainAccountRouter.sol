// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {WorldChainAccountVerifier} from "./interfaces/IWorldChainAccountManager.sol";
import {IWorldChainAccountRouter} from "./interfaces/IWorldChainAccountRouter.sol";
import {IWorldChainSessionVerifier} from "./interfaces/IWorldChainSessionVerifier.sol";

/// @title WorldChainAccountRouter
/// @author 0xOsiris, World Contributors
/// @notice The account router is the only validation target called by the manager. It dispatches
///         to configured verifier implementations using a single controlled `DELEGATECALL`, so
///         verifier implementations execute against the account's storage.
/// @custom:security-contact security@toolsforhumanity.com
contract WorldChainAccountRouter is IWorldChainAccountRouter {
    /// @inheritdoc IWorldChainAccountRouter
    function installAdmin(WorldChainAccountVerifier calldata admin) external {}

    /// @inheritdoc IWorldChainAccountRouter
    function installKeyRing(WorldChainAccountVerifier[] calldata sessionVerifiers) external {}

    /// @inheritdoc IWorldChainAccountRouter
    function isValidSignatureForAdmin(bytes32 hash, bytes calldata signature)
        external
        view
        returns (bytes4 magicValue)
    {}

    /// @inheritdoc IWorldChainAccountRouter
    function isValidSignatureForVerifier(address verifier, bytes32 hash, bytes calldata signature)
        external
        view
        returns (bytes4 magicValue)
    {}

    /// @inheritdoc IWorldChainAccountRouter
    function evaluateSessionPolicyForVerifier(
        address verifier,
        IWorldChainSessionVerifier.ExecutionTraceContext calldata context
    ) external view returns (bool allowed) {}
}
