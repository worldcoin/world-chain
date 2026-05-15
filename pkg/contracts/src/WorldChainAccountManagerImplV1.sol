// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";

import {Base} from "./abstract/Base.sol";
import {IWorldChainAccountManager} from "./interfaces/IWorldChainAccountManager.sol";
import {IWorldChainAccountRouter} from "./interfaces/IWorldChainAccountRouter.sol";
import {VerifierDispatch} from "./libraries/VerifierDispatch.sol";
import {WorldChainAccountConstants} from "./libraries/WorldChainAccountConstants.sol";
import {WorldChainAccountHashes} from "./libraries/WorldChainAccountHashes.sol";
import {WorldChainAccountRouter} from "./WorldChainAccountRouter.sol";
import {
    AccountAlreadyExists,
    AccountDoesNotExist,
    ActivationParametersAlreadyFrozen,
    AddressZero,
    AdminAuthorizationFailed,
    AdminAuthorizationTooLarge,
    AdminVerifierZero,
    DuplicateSessionVerifier,
    EmptySessionVerifierSet,
    InstallationTooLarge,
    KeyRingUnchanged,
    SessionVerifierZero,
    StaleKeyRingHash,
    TooManySessionVerifiers,
    UnauthorizedSessionVerifier,
    VerifierUndeployed
} from "./libraries/WorldChainAccountErrors.sol";

/// @title WorldChainAccountManagerImplV1
/// @author Worldcoin
/// @notice WIP-1001 World Chain Account Manager: the Solidity reference implementation of the
///         predeploy that owns account-address derivation, admin-signer binding, and
///         key-ring lifecycle for every WIP-1001 account on the chain.
/// @dev All upgrades to {WorldChainAccountManager} after initial deployment MUST inherit this
///      contract to preserve EIP-7201 namespaced storage.
///
///      The Solidity reference impl differs from the eventual native predeploy in one place:
///      the spec defines a custom account-address derivation that is not reachable from
///      Solidity's `CREATE` / `CREATE2`. The reference impl computes the canonical spec
///      address — which is what all hashes commit to — and then deploys the actual router via
///      `CREATE2` at a separate, deterministic address tracked in {getAccountRouter}. In
///      production the native predeploy installs the router runtime code directly at the
///      canonical spec address; the manager's behaviour is otherwise identical in either mode.
/// @custom:security-contact security@toolsforhumanity.com
contract WorldChainAccountManagerImplV1 is IWorldChainAccountManager, Base, ReentrancyGuardTransient {
    ///////////////////////////////////////////////////////////////////////////////
    ///                          EIP-7201 NAMESPACED STORAGE                    ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @custom:storage-location erc7201:worldchain.accountmanager.v1
    struct ManagerStorage {
        // Activation parameters. May be retuned by the owner until {activationParametersFrozen}
        // is set; after that, all setters revert.
        uint64 eip1271ValidationGasLimit;
        uint32 maxVerifierInstallDataBytes;
        uint32 maxAdminAuthorizationBytes;
        bool activationParametersFrozen;
        // The runtime code hash of the {WorldChainAccountRouter} this manager deploys. Folded
        // into account-address derivation and admin operation hashes; set once at initialise.
        bytes32 accountRouterCodeHash;
        // Authoritative per-account state.
        mapping(address account => AccountStorage record) accounts;
    }

    /// @dev Per-account record. `adminVerifier == address(0)` is the canonical
    ///      "account does not exist" sentinel.
    struct AccountStorage {
        address adminVerifier;
        uint64 adminNonce;
        uint64 transactionNonce;
        bytes32 accountSalt;
        bytes32 keyRingHash;
        bytes adminInstallation;
        // Solidity reference impl only: actual `CREATE2`-deployed router address. In the
        // production predeploy this equals the spec-derived account address.
        address router;
        WorldChainAccountVerifier[] sessionVerifiers;
        mapping(address verifier => uint256 indexPlusOne) sessionVerifierIndexPlusOne;
    }

    // keccak256(abi.encode(uint256(keccak256("worldchain.accountmanager.v1")) - 1))
    //     & ~bytes32(uint256(0xff));
    bytes32 private constant MANAGER_STORAGE_LOCATION =
        keccak256(abi.encode(uint256(keccak256("worldchain.accountmanager.v1")) - 1)) & ~bytes32(uint256(0xff));

    function _$() private pure returns (ManagerStorage storage $) {
        bytes32 slot = MANAGER_STORAGE_LOCATION;
        assembly {
            $.slot := slot
        }
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              ACTIVATION KEYS                            ///
    ///////////////////////////////////////////////////////////////////////////////

    bytes32 private constant KEY_EIP1271_VALIDATION_GAS_LIMIT = keccak256("eip1271ValidationGasLimit");
    bytes32 private constant KEY_MAX_VERIFIER_INSTALL_DATA_BYTES = keccak256("maxVerifierInstallDataBytes");
    bytes32 private constant KEY_MAX_ADMIN_AUTHORIZATION_BYTES = keccak256("maxAdminAuthorizationBytes");

    ///////////////////////////////////////////////////////////////////////////////
    ///                              CONSTRUCTION                               ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Locks the implementation. State setup happens in `initialize` against the proxy.
    constructor() {
        _disableInitializers();
    }

    /// @notice Initialises the proxy.
    /// @param owner_ The contract owner (Ownable2Step).
    function initialize(address owner_) external reinitializer(1) {
        if (owner_ == address(0)) revert AddressZero();
        __Base_init(owner_);

        ManagerStorage storage $ = _$();
        $.eip1271ValidationGasLimit = WorldChainAccountConstants.DEFAULT_EIP1271_VALIDATION_GAS_LIMIT;
        $.maxVerifierInstallDataBytes = WorldChainAccountConstants.DEFAULT_MAX_VERIFIER_INSTALL_DATA_BYTES;
        $.maxAdminAuthorizationBytes = WorldChainAccountConstants.DEFAULT_MAX_ADMIN_AUTHORIZATION_BYTES;

        // Capture the runtime code hash of WorldChainAccountRouter. `runtimeCode` is the
        // post-constructor bytecode and is byte-identical across every router instance
        // because the router holds its per-deployment parameters in storage rather than
        // immutables.
        bytes memory rc = type(WorldChainAccountRouter).runtimeCode;
        bytes32 codeHash;
        assembly ("memory-safe") {
            codeHash := keccak256(add(rc, 0x20), mload(rc))
        }
        $.accountRouterCodeHash = codeHash;

        emit WorldChainAccountManagerInitialized(
            owner_, $.eip1271ValidationGasLimit, $.maxVerifierInstallDataBytes, $.maxAdminAuthorizationBytes
        );
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                          ACTIVATION-PARAMETER ADMIN                     ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Updates the EIP-1271 validation gas limit. Reverts after activation parameters
    ///         are frozen.
    function setEip1271ValidationGasLimit(uint64 newValue) external onlyProxy onlyOwner {
        ManagerStorage storage $ = _$();
        _requireUnfrozen($);
        uint64 old = $.eip1271ValidationGasLimit;
        $.eip1271ValidationGasLimit = newValue;
        emit ActivationParameterUpdated(KEY_EIP1271_VALIDATION_GAS_LIMIT, old, newValue);
    }

    /// @notice Updates the maximum verifier `installation` size. Reverts after activation
    ///         parameters are frozen.
    function setMaxVerifierInstallDataBytes(uint32 newValue) external onlyProxy onlyOwner {
        ManagerStorage storage $ = _$();
        _requireUnfrozen($);
        uint32 old = $.maxVerifierInstallDataBytes;
        $.maxVerifierInstallDataBytes = newValue;
        emit ActivationParameterUpdated(KEY_MAX_VERIFIER_INSTALL_DATA_BYTES, old, newValue);
    }

    /// @notice Updates the maximum admin authorisation size. Reverts after activation
    ///         parameters are frozen.
    function setMaxAdminAuthorizationBytes(uint32 newValue) external onlyProxy onlyOwner {
        ManagerStorage storage $ = _$();
        _requireUnfrozen($);
        uint32 old = $.maxAdminAuthorizationBytes;
        $.maxAdminAuthorizationBytes = newValue;
        emit ActivationParameterUpdated(KEY_MAX_ADMIN_AUTHORIZATION_BYTES, old, newValue);
    }

    /// @notice Permanently freezes the activation parameters. Irreversible.
    function freezeActivationParameters() external onlyProxy onlyOwner {
        ManagerStorage storage $ = _$();
        _requireUnfrozen($);
        $.activationParametersFrozen = true;
        emit ActivationParametersFrozen();
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                  CREATE                                 ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @inheritdoc IWorldChainAccountManager
    function create(
        WorldChainAccountVerifier calldata admin,
        bytes32 accountSalt,
        WorldChainAccountVerifier[] calldata initialSessionVerifiers,
        bytes calldata adminAuthorization
    ) external virtual override onlyProxy nonReentrant returns (address account) {
        ManagerStorage storage $ = _$();

        _validateCreateInputs($, admin, initialSessionVerifiers, adminAuthorization);

        bytes32 adminHash_ = WorldChainAccountHashes.adminHash(admin);
        account = WorldChainAccountHashes.deriveAccount(
            block.chainid, address(this), $.accountRouterCodeHash, adminHash_, accountSalt
        );

        AccountStorage storage acct = $.accounts[account];
        if (acct.adminVerifier != address(0)) revert AccountAlreadyExists(account);

        bytes32 keyRingHash_ = WorldChainAccountHashes.keyRingHash(initialSessionVerifiers);

        // Deploy the router via CREATE2 with the canonical account address as the salt. In
        // the predeploy this step is replaced by the native bytecode installation at
        // `account` itself.
        WorldChainAccountRouter router = new WorldChainAccountRouter{salt: bytes32(uint256(uint160(account)))}(
            address(this), $.eip1271ValidationGasLimit
        );

        // Provision the router. installAdmin / installKeyRing are gated to msg.sender == this.
        router.installAdmin(admin);
        router.installKeyRing(initialSessionVerifiers);

        // Verify the admin authorisation inside a fixed-gas STATICCALL.
        _verifyCreateAuthorization(
            $, address(router), account, adminHash_, accountSalt, keyRingHash_, adminAuthorization
        );

        // Persist authoritative manager-side record.
        acct.adminVerifier = admin.verifier;
        acct.adminInstallation = admin.installation;
        acct.accountSalt = accountSalt;
        acct.keyRingHash = keyRingHash_;
        acct.router = address(router);
        _writeSessionVerifiers(acct, initialSessionVerifiers);

        emit AccountCreated(account, admin.verifier, adminHash_, accountSalt, keyRingHash_);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                SET KEY RING                             ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @inheritdoc IWorldChainAccountManager
    function setKeyRing(
        address account,
        bytes32 expectedCurrentKeyRingHash,
        WorldChainAccountVerifier[] calldata sessionVerifiers,
        bytes calldata adminAuthorization
    ) external virtual override onlyProxy nonReentrant {
        ManagerStorage storage $ = _$();
        AccountStorage storage acct = $.accounts[account];
        if (acct.adminVerifier == address(0)) revert AccountDoesNotExist(account);

        if (adminAuthorization.length > $.maxAdminAuthorizationBytes) {
            revert AdminAuthorizationTooLarge(adminAuthorization.length, $.maxAdminAuthorizationBytes);
        }
        if (expectedCurrentKeyRingHash != acct.keyRingHash) {
            revert StaleKeyRingHash(expectedCurrentKeyRingHash, acct.keyRingHash);
        }
        _validateSessionVerifiersForCreate($, sessionVerifiers, $.maxVerifierInstallDataBytes);

        bytes32 newKeyRingHash_ = WorldChainAccountHashes.keyRingHash(sessionVerifiers);
        if (newKeyRingHash_ == acct.keyRingHash) revert KeyRingUnchanged(newKeyRingHash_);

        // Verify the admin authorisation; isolated into a helper so the stack stays shallow.
        _verifySetKeyRingAuthorization($, acct, account, acct.keyRingHash, newKeyRingHash_, adminAuthorization);

        // Apply on-router.
        IWorldChainAccountRouter(acct.router).installKeyRing(sessionVerifiers);

        // Apply manager-side state.
        bytes32 previousKeyRingHash = acct.keyRingHash;
        _clearSessionVerifiers(acct);
        _writeSessionVerifiers(acct, sessionVerifiers);
        acct.keyRingHash = newKeyRingHash_;
        uint64 newNonce;
        unchecked {
            newNonce = acct.adminNonce + 1;
        }
        acct.adminNonce = newNonce;

        emit KeyRingSet(account, previousKeyRingHash, newKeyRingHash_, newNonce);
    }

    /// @dev Computes the `setKeyRing` authorisation hash and verifies it through the router.
    ///      Extracted from {setKeyRing} purely to keep its stack frame shallow enough to
    ///      compile without `via_ir`.
    function _verifySetKeyRingAuthorization(
        ManagerStorage storage $,
        AccountStorage storage acct,
        address account,
        bytes32 currentKeyRingHash,
        bytes32 newKeyRingHash_,
        bytes calldata adminAuthorization
    ) private view {
        bytes32 setHash_ = WorldChainAccountHashes.setKeyRingHash(
            block.chainid, address(this), account, acct.adminNonce, currentKeyRingHash, newKeyRingHash_
        );
        _verifyAdmin(acct.router, $.eip1271ValidationGasLimit, setHash_, adminAuthorization);
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                   VIEWS                                 ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @inheritdoc IWorldChainAccountManager
    function getAdmin(address account) external view override returns (WorldChainAccountVerifier memory) {
        AccountStorage storage acct = _$().accounts[account];
        if (acct.adminVerifier == address(0)) revert AccountDoesNotExist(account);
        return WorldChainAccountVerifier({verifier: acct.adminVerifier, installation: acct.adminInstallation});
    }

    /// @inheritdoc IWorldChainAccountManager
    function getAdminNonce(address account) external view override returns (uint64) {
        AccountStorage storage acct = _$().accounts[account];
        if (acct.adminVerifier == address(0)) revert AccountDoesNotExist(account);
        return acct.adminNonce;
    }

    /// @inheritdoc IWorldChainAccountManager
    function getTransactionNonce(address account) external view override returns (uint64) {
        AccountStorage storage acct = _$().accounts[account];
        if (acct.adminVerifier == address(0)) revert AccountDoesNotExist(account);
        return acct.transactionNonce;
    }

    /// @inheritdoc IWorldChainAccountManager
    function getKeyRingHash(address account) external view override returns (bytes32) {
        AccountStorage storage acct = _$().accounts[account];
        if (acct.adminVerifier == address(0)) revert AccountDoesNotExist(account);
        return acct.keyRingHash;
    }

    /// @inheritdoc IWorldChainAccountManager
    function getSessionVerifiers(address account) external view override returns (WorldChainAccountVerifier[] memory) {
        AccountStorage storage acct = _$().accounts[account];
        if (acct.adminVerifier == address(0)) revert AccountDoesNotExist(account);
        return acct.sessionVerifiers;
    }

    /// @inheritdoc IWorldChainAccountManager
    function isAuthorizedSessionVerifier(address account, address sessionVerifier)
        external
        view
        override
        returns (bool)
    {
        return _$().accounts[account].sessionVerifierIndexPlusOne[sessionVerifier] != 0;
    }

    /// @inheritdoc IWorldChainAccountManager
    function getAuthorizedSessionVerifier(address account, address sessionVerifier)
        external
        view
        override
        returns (WorldChainAccountVerifier memory)
    {
        AccountStorage storage acct = _$().accounts[account];
        uint256 indexPlusOne = acct.sessionVerifierIndexPlusOne[sessionVerifier];
        if (indexPlusOne == 0) revert UnauthorizedSessionVerifier(sessionVerifier);
        return acct.sessionVerifiers[indexPlusOne - 1];
    }

    /// @inheritdoc IWorldChainAccountManager
    function getAccountRouterCodeHash() external view override returns (bytes32) {
        return _$().accountRouterCodeHash;
    }

    /// @inheritdoc IWorldChainAccountManager
    function getAccountRouter(address account) external view override returns (address) {
        return _$().accounts[account].router;
    }

    /// @notice Returns the current EIP-1271 validation gas limit.
    function eip1271ValidationGasLimit() external view returns (uint64) {
        return _$().eip1271ValidationGasLimit;
    }

    /// @notice Returns the current maximum verifier `installation` byte length.
    function maxVerifierInstallDataBytes() external view returns (uint32) {
        return _$().maxVerifierInstallDataBytes;
    }

    /// @notice Returns the current maximum `adminAuthorization` byte length.
    function maxAdminAuthorizationBytes() external view returns (uint32) {
        return _$().maxAdminAuthorizationBytes;
    }

    /// @notice Returns whether activation parameters have been frozen.
    function activationParametersFrozen() external view returns (bool) {
        return _$().activationParametersFrozen;
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                                INTERNAL                                 ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @dev Validates a candidate session-verifier set for `create` / `setKeyRing`.
    function _validateSessionVerifiersForCreate(
        ManagerStorage storage, /* $ — referenced via parameter for symmetry; unused */
        WorldChainAccountVerifier[] calldata sessionVerifiers,
        uint32 maxInstall
    ) private view {
        uint256 len = sessionVerifiers.length;
        if (len == 0) revert EmptySessionVerifierSet();
        uint256 max = WorldChainAccountConstants.MAX_SESSION_VERIFIERS;
        if (len > max) revert TooManySessionVerifiers(len, max);

        // Use a memory-local index map to detect duplicates without touching storage.
        // Bounded by MAX_SESSION_VERIFIERS so the quadratic scan is cheap.
        for (uint256 i; i < len; ++i) {
            address verifier = sessionVerifiers[i].verifier;
            if (verifier == address(0)) revert SessionVerifierZero();
            if (verifier.code.length == 0) revert VerifierUndeployed(verifier);
            uint256 installLen = sessionVerifiers[i].installation.length;
            if (installLen > maxInstall) revert InstallationTooLarge(installLen, maxInstall);
            for (uint256 j; j < i; ++j) {
                if (sessionVerifiers[j].verifier == verifier) {
                    revert DuplicateSessionVerifier(verifier);
                }
            }
        }
    }

    /// @dev Writes a freshly validated session-verifier set into account storage. Caller MUST
    ///      have cleared any prior state via {_clearSessionVerifiers}.
    function _writeSessionVerifiers(AccountStorage storage acct, WorldChainAccountVerifier[] calldata sessionVerifiers)
        private
    {
        uint256 len = sessionVerifiers.length;
        for (uint256 i; i < len; ++i) {
            acct.sessionVerifiers.push(sessionVerifiers[i]);
            acct.sessionVerifierIndexPlusOne[sessionVerifiers[i].verifier] = i + 1;
        }
    }

    /// @dev Clears the existing session-verifier set, index map included.
    function _clearSessionVerifiers(AccountStorage storage acct) private {
        WorldChainAccountVerifier[] storage list = acct.sessionVerifiers;
        uint256 len = list.length;
        for (uint256 i; i < len; ++i) {
            delete acct.sessionVerifierIndexPlusOne[list[i].verifier];
        }
        // Truncate the array. `delete` on a storage array with dynamic elements iterates the
        // elements, which here means visiting each `bytes installation`. Bounded by
        // MAX_SESSION_VERIFIERS, so acceptable.
        while (list.length > 0) {
            list.pop();
        }
    }

    /// @dev STATICCALLs `router.isValidSignatureForAdmin(hash, signature)` with exactly
    ///      `gasLimit` gas. Reverts {AdminAuthorizationFailed} on any non-magic return.
    function _verifyAdmin(address router, uint64 gasLimit, bytes32 hash, bytes calldata signature) private view {
        bytes memory callData =
            abi.encodeWithSelector(IWorldChainAccountRouter.isValidSignatureForAdmin.selector, hash, signature);
        if (!VerifierDispatch.staticcallEip1271(router, gasLimit, callData)) {
            revert AdminAuthorizationFailed();
        }
    }

    /// @dev Reverts when activation parameters have been frozen.
    function _requireUnfrozen(ManagerStorage storage $) private view {
        if ($.activationParametersFrozen) revert ActivationParametersAlreadyFrozen();
    }

    /// @dev Performs the upfront input validation for {create}. Extracted purely to keep the
    ///      {create} stack frame shallow enough to compile without `via_ir`.
    function _validateCreateInputs(
        ManagerStorage storage $,
        WorldChainAccountVerifier calldata admin,
        WorldChainAccountVerifier[] calldata initialSessionVerifiers,
        bytes calldata adminAuthorization
    ) private view {
        uint32 maxInstall = $.maxVerifierInstallDataBytes;
        uint32 maxAuth = $.maxAdminAuthorizationBytes;
        if (adminAuthorization.length > maxAuth) {
            revert AdminAuthorizationTooLarge(adminAuthorization.length, maxAuth);
        }
        if (admin.verifier == address(0)) revert AdminVerifierZero();
        if (admin.verifier.code.length == 0) revert VerifierUndeployed(admin.verifier);
        if (admin.installation.length > maxInstall) {
            revert InstallationTooLarge(admin.installation.length, maxInstall);
        }
        _validateSessionVerifiersForCreate($, initialSessionVerifiers, maxInstall);
    }

    /// @dev Computes the `create` authorisation hash and verifies it through the router.
    ///      Extracted from {create} to keep the stack frame shallow.
    function _verifyCreateAuthorization(
        ManagerStorage storage $,
        address router,
        address account,
        bytes32 adminHash_,
        bytes32 accountSalt,
        bytes32 keyRingHash_,
        bytes calldata adminAuthorization
    ) private view {
        bytes32 createHash_ = WorldChainAccountHashes.createHash(
            block.chainid, address(this), $.accountRouterCodeHash, account, adminHash_, accountSalt, keyRingHash_
        );
        _verifyAdmin(router, $.eip1271ValidationGasLimit, createHash_, adminAuthorization);
    }
}
