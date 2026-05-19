// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Address} from "@openzeppelin/contracts/utils/Address.sol";
import {IERC1271} from "@openzeppelin/contracts/interfaces/IERC1271.sol";

import {Dispatch} from "./abstract/Dispatch.sol";
import {KeyRingStore} from "./abstract/KeyRingStore.sol";
import {WorldChainAccountVerifier} from "./interfaces/IWorldChainAccountManager.sol";
import {IWorldChainAccount} from "./interfaces/IWorldChainAccount.sol";
import {IWorldChainAccountRouterErrors} from "./interfaces/IWorldChainAccountRouterErrors.sol";
import {IWorldChainSessionVerifier} from "./interfaces/IWorldChainSessionVerifier.sol";

/// @title WorldChainAccount
/// @author 0xOsiris, World Contributors
/// @notice Implementation behind `WorldChainAccountUpgradeableBeacon` and reached at every account address
///         through `WorldChainAccountBeaconProxy`
/// @custom:security-contact security@toolsforhumanity.com
contract WorldChainAccount is KeyRingStore, Dispatch, IWorldChainAccount, IWorldChainAccountRouterErrors {
    using Address for address;

    /// @notice The contract version.
    /// @custom:semver v1.0.0
    uint8 public constant override VERSION = 1;

    /// @inheritdoc IWorldChainAccount
    /// @dev Baked into the implementation bytecode. The beacon owner can rotate this only by
    ///      shipping a new implementation that compiles in a different value.
    address public immutable override MANAGER;

    constructor(address manager) {
        MANAGER = manager;
    }

    /// @notice Restricts the call to `WORLD_CHAIN_ACCOUNT_MANAGER`.
    modifier onlyManager() {
        if (msg.sender != MANAGER) revert CallerNotManager();
        _;
    }

    /// @inheritdoc IWorldChainAccount
    function ADMIN_VERIFIER() external view override returns (address) {
        return _adminVerifier();
    }

    /// @inheritdoc IWorldChainAccount
    function KEYRING_HASH() external view override returns (bytes32) {
        return _keyringHash();
    }

    /// @inheritdoc IWorldChainAccount
    function sessionKeyRing(address verifier) external view override returns (bytes32) {
        return _sessionKeyRing(verifier);
    }

    /// @inheritdoc IWorldChainAccount
    function installAdmin(WorldChainAccountVerifier calldata admin) external override onlyManager {
        _installAdmin(admin);
    }

    /// @inheritdoc IWorldChainAccount
    function installKeyRing(WorldChainAccountVerifier[] calldata sessionVerifiers) external override onlyManager {
        _installKeyRing(sessionVerifiers);
    }

    /// @inheritdoc IWorldChainAccount
    function isValidSignatureForAdmin(bytes32 hash, bytes calldata signature)
        external
        override
        returns (bytes4 magicValue)
    {
        address admin = _adminVerifier();
        if (admin == address(0)) revert AdminNotInstalled();
        bytes memory ret = admin.functionDelegateCall(abi.encodeCall(IERC1271.isValidSignature, (hash, signature)));
        magicValue = abi.decode(ret, (bytes4));
    }

    /// @inheritdoc IWorldChainAccount
    function isValidSignatureForVerifier(address verifier, bytes32 hash, bytes calldata signature)
        external
        override
        sessionInstalled(verifier)
        returns (bytes4 magicValue)
    {
        bytes memory ret = verifier.functionDelegateCall(abi.encodeCall(IERC1271.isValidSignature, (hash, signature)));
        magicValue = abi.decode(ret, (bytes4));
    }

    /// @inheritdoc IWorldChainAccount
    function evaluateSessionPolicyForVerifier(
        address verifier,
        IWorldChainSessionVerifier.ExecutionTraceContext calldata context
    ) external override sessionInstalled(verifier) returns (bool allowed) {
        bytes memory ret =
            verifier.functionDelegateCall(abi.encodeCall(IWorldChainSessionVerifier.evaluateSessionPolicy, (context)));
        allowed = abi.decode(ret, (bool));
    }
}
