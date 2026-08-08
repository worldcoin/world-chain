// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {IPaymaster} from "@account-abstraction/interfaces/IPaymaster.sol";
import {IEntryPoint} from "@account-abstraction/interfaces/IEntryPoint.sol";
import {PackedUserOperation} from "@account-abstraction/interfaces/PackedUserOperation.sol";
import {UserOperationLib} from "@account-abstraction/core/UserOperationLib.sol";
import {IERC165} from "@openzeppelin/contracts/utils/introspection/IERC165.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";

/**
 * @title BasePaymasterUpgradeable
 * @notice Initializable port of eth-infinitism's `BasePaymaster`, for paymasters
 *         deployed behind a proxy.
 *
 * @dev The upstream base takes the EntryPoint in its constructor and stores it in an
 *      `immutable`, which lives in the *implementation's* bytecode. Behind a proxy
 *      that value is still readable (immutables are baked into code, not storage),
 *      but it can then only be changed by shipping a new implementation, and the
 *      constructor's `Ownable(msg.sender)` would make the *deployer of the
 *      implementation* the owner rather than the proxy's initializer. So the
 *      EntryPoint moves into storage and ownership comes from `__Ownable_init`.
 *
 *      Storage is ERC-7201 namespaced, so this base occupies no slots in the
 *      inheriting contract's own layout and can gain fields later without shifting
 *      anything below it. The external API is byte-for-byte the upstream one.
 */
abstract contract BasePaymasterUpgradeable is IPaymaster, Initializable, OwnableUpgradeable {
    uint256 internal constant PAYMASTER_VALIDATION_GAS_OFFSET = UserOperationLib.PAYMASTER_VALIDATION_GAS_OFFSET;
    uint256 internal constant PAYMASTER_POSTOP_GAS_OFFSET = UserOperationLib.PAYMASTER_POSTOP_GAS_OFFSET;
    uint256 internal constant PAYMASTER_DATA_OFFSET = UserOperationLib.PAYMASTER_DATA_OFFSET;

    /// @custom:storage-location erc7201:worldchain.storage.BasePaymaster
    struct BasePaymasterStorage {
        IEntryPoint entryPoint;
    }

    /// @dev keccak256(abi.encode(uint256(keccak256("worldchain.storage.BasePaymaster")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant BASE_PAYMASTER_STORAGE_LOCATION =
        0x2bffca0ad219ef713505d94e0f0abd978c9664fc2c37a7bc6b63893298597700;

    error InvalidEntryPoint();

    function _getBasePaymasterStorage() private pure returns (BasePaymasterStorage storage $) {
        assembly {
            $.slot := BASE_PAYMASTER_STORAGE_LOCATION
        }
    }

    /// @notice The EntryPoint this paymaster serves.
    function entryPoint() public view returns (IEntryPoint) {
        return _getBasePaymasterStorage().entryPoint;
    }

    /**
     * @dev Initializes ownership and the EntryPoint. Call from the inheriting
     *      contract's `initialize`.
     */
    function __BasePaymaster_init(IEntryPoint _entryPoint, address initialOwner) internal onlyInitializing {
        __Ownable_init(initialOwner);
        _validateEntryPointInterface(_entryPoint);
        _getBasePaymasterStorage().entryPoint = _entryPoint;
    }

    /// @dev Sanity check that `_entryPoint` really is an EntryPoint of this version.
    function _validateEntryPointInterface(IEntryPoint _entryPoint) internal view virtual {
        if (!IERC165(address(_entryPoint)).supportsInterface(type(IEntryPoint).interfaceId)) {
            revert InvalidEntryPoint();
        }
    }

    /// @inheritdoc IPaymaster
    function validatePaymasterUserOp(PackedUserOperation calldata userOp, bytes32 userOpHash, uint256 maxCost)
        external
        override
        returns (bytes memory context, uint256 validationData)
    {
        _requireFromEntryPoint();
        return _validatePaymasterUserOp(userOp, userOpHash, maxCost);
    }

    function _validatePaymasterUserOp(PackedUserOperation calldata userOp, bytes32 userOpHash, uint256 maxCost)
        internal
        virtual
        returns (bytes memory context, uint256 validationData);

    /// @inheritdoc IPaymaster
    function postOp(PostOpMode mode, bytes calldata context, uint256 actualGasCost, uint256 actualUserOpFeePerGas)
        external
        override
    {
        _requireFromEntryPoint();
        _postOp(mode, context, actualGasCost, actualUserOpFeePerGas);
    }

    function _postOp(PostOpMode mode, bytes calldata context, uint256 actualGasCost, uint256 actualUserOpFeePerGas)
        internal
        virtual
    {
        (mode, context, actualGasCost, actualUserOpFeePerGas); // unused params
        revert("must override");
    }

    /// @notice Add to the deposit that pays for sponsored gas. Permissionless.
    function deposit() public payable {
        entryPoint().depositTo{value: msg.value}(address(this));
    }

    /// @notice Withdraw from the deposit.
    function withdrawTo(address payable withdrawAddress, uint256 amount) public onlyOwner {
        entryPoint().withdrawTo(withdrawAddress, amount);
    }

    /// @notice Stake on the EntryPoint. Required by ERC-7562 storage rules.
    /// @param unstakeDelaySec Unstake delay; can only be increased.
    function addStake(uint32 unstakeDelaySec) external payable onlyOwner {
        entryPoint().addStake{value: msg.value}(unstakeDelaySec);
    }

    /// @notice This paymaster's current EntryPoint deposit.
    function getDeposit() public view returns (uint256) {
        return entryPoint().balanceOf(address(this));
    }

    /// @notice Start the unstake clock. The paymaster cannot serve ops once unlocked.
    function unlockStake() external onlyOwner {
        entryPoint().unlockStake();
    }

    /// @notice Withdraw the stake, once unlocked and the delay has elapsed.
    function withdrawStake(address payable withdrawAddress) external onlyOwner {
        entryPoint().withdrawStake(withdrawAddress);
    }

    function _requireFromEntryPoint() internal view virtual {
        require(msg.sender == address(entryPoint()), "Sender not EntryPoint");
    }
}
