// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Test.sol";
import "@account-abstraction/contracts/interfaces/PackedUserOperation.sol";
import {MockWorldIDGroups} from "./mocks/MockWorldIDGroups.sol";
import {IPBHEntryPoint} from "../src/interfaces/IPBHEntryPoint.sol";
import {PBHSignatureAggregator} from "../src/PBHSignatureAggregator.sol";
import {IEntryPoint} from "@account-abstraction/contracts/interfaces/IEntryPoint.sol";
import {IAggregator} from "@account-abstraction/contracts/interfaces/IAggregator.sol";
import {IWorldID} from "@world-id-contracts/interfaces/IWorldID.sol";
import {IAccount} from "@account-abstraction/contracts/interfaces/IAccount.sol";
import {PBHEntryPointImplV1} from "../src/PBHEntryPointImplV1.sol";
import {PBHEntryPoint} from "../src/PBHEntryPoint.sol";
import {Safe} from "@safe-global/safe-contracts/contracts/Safe.sol";
import {SafeProxyFactory} from "@safe-global/safe-contracts/contracts/proxies/SafeProxyFactory.sol";
import {SafeProxy} from "@safe-global/safe-contracts/contracts/proxies/SafeProxy.sol";
import {Enum} from "@safe-global/safe-contracts/contracts/common/Enum.sol";
import {SafeModuleSetup} from "@4337/SafeModuleSetup.sol";
import {PBHSafe4337Module} from "../src/PBH4337Module.sol";
import {Mock4337Module} from "./mocks/Mock4337Module.sol";
import {Safe4337Module} from "@4337/Safe4337Module.sol";
import {IAccount} from "@account-abstraction/contracts/interfaces/IAccount.sol";
import {MockAccount} from "./mocks/MockAccount.sol";
import {MockEIP1271SignatureValidator} from "./mocks/MockEIP1271SignatureValidator.sol";

/// @title Test Setup Contract.
/// @author Worldcoin
/// @dev This test suite tests both the proxy and the functionality of the underlying implementation
///      so as to test everything in the context of how it will be deployed.
contract TestSetup is Test {
    ///////////////////////////////////////////////////////////////////////////////
    ///                                TEST DATA                                ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice The 4337 Entry Point on Ethereum Mainnet.
    IEntryPoint internal entryPoint = IEntryPoint(address(0x0000000071727De22E5E9d8BAf0edAc6f37da032));
    /// @notice The PBHEntryPoint contract.
    IPBHEntryPoint public pbhEntryPoint;
    /// @notice The PBHSignatureAggregator contract.
    IAggregator public pbhAggregator;
    /// @notice The Mock World ID Groups contract.
    MockWorldIDGroups public worldIDGroups;

    Mock4337Module public pbh4337Module;
    Safe public singleton;
    Safe public safe;
    SafeProxyFactory public factory;
    SafeModuleSetup public moduleSetup;

    IAccount public mockSafe;

    address public mockEIP1271SignatureValidator;

    address public safeOwner;
    uint256 public constant safeOwnerKey = 0x1234;
    address public OWNER = address(0xc0ffee);
    address public pbhEntryPointImpl;
    address public immutable thisAddress = address(this);
    address public constant nullAddress = address(0);
    address public constant MULTICALL3 = 0xcA11bde05977b3631167028862bE2a173976CA11;

    uint192 public constant PBH_NONCE_KEY = 1123123123;

    uint8 public constant MAX_NUM_PBH_PER_MONTH = 30;
    ///////////////////////////////////////////////////////////////////////////////
    ///                            TEST ORCHESTRATION                           ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice This function runs before every single test.
    /// @dev It is run before every single iteration of a property-based fuzzing test.
    function setUp() public virtual {
        safeOwner = vm.addr(safeOwnerKey);
        vm.startPrank(OWNER);
        deployWorldIDGroups();
        deployPBHEntryPoint(worldIDGroups, entryPoint);
        deployPBHSignatureAggregator(address(pbhEntryPoint));
        deploySafeAndModule(address(pbhAggregator), 1);
        deployMockSafe(address(pbhAggregator), 1);
        deployEIP1271SignatureValidator();
        vm.stopPrank();

        // Label the addresses for better errors.
        vm.label(address(entryPoint), "ERC-4337 Entry Point");
        vm.label(address(pbhAggregator), "PBH Signature Aggregator");
        vm.label(address(safe), "Safe");
        vm.label(address(worldIDGroups), "Mock World ID Groups");
        vm.label(address(pbhEntryPoint), "PBH Entry Point");
        vm.label(pbhEntryPointImpl, "PBH Entry Point Impl V1");
        vm.label(address(pbh4337Module), "PBH 4337 Module");
        vm.label(address(factory), "Safe Proxy Factory");
        vm.label(address(moduleSetup), "Safe Module Setup");
        vm.label(address(singleton), "Safe Singleton");
        vm.label(mockEIP1271SignatureValidator, "Mock EIP1271 Signature Validator");

        vm.deal(address(this), type(uint128).max);
        vm.deal(address(pbh4337Module), type(uint128).max);
        vm.deal(address(safe), type(uint128).max);
        // Deposit some funds into the Entry Point from the PBH 4337 Module.
        entryPoint.depositTo{value: 10 ether}(address(safe));
    }

    ///////////////////////////////////////////////////////////////////////////////
    ///                              TEST UTILITIES                             ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Initializes a new router.
    /// @dev It is constructed in the globals.
    ///
    /// @param initialGroupAddress The initial group's identity manager.
    /// @param initialEntryPoint The initial entry point.
    function deployPBHEntryPoint(IWorldID initialGroupAddress, IEntryPoint initialEntryPoint) public {
        pbhEntryPointImpl = address(new PBHEntryPointImplV1());

        bytes memory initCallData = abi.encodeCall(
            PBHEntryPointImplV1.initialize, (initialGroupAddress, initialEntryPoint, MAX_NUM_PBH_PER_MONTH, MULTICALL3)
        );
        vm.expectEmit(true, true, true, true);
        emit PBHEntryPointImplV1.PBHEntryPointImplInitialized(
            initialGroupAddress, initialEntryPoint, MAX_NUM_PBH_PER_MONTH, MULTICALL3
        );
        pbhEntryPoint = IPBHEntryPoint(address(new PBHEntryPoint(pbhEntryPointImpl, initCallData)));
    }

    /// @notice Deploys a new EIP1271 Signature Validator.
    function deployEIP1271SignatureValidator() public {
        mockEIP1271SignatureValidator = address(new MockEIP1271SignatureValidator());
    }

    /// @notice Initializes a new safe account.
    /// @dev It is constructed in the globals.
    function deployMockSafe(address _pbhSignatureAggregator, uint256 threshold) public {
        mockSafe = new MockAccount(_pbhSignatureAggregator, threshold);
    }

    /// @notice Initializes a new PBHSignatureAggregator.
    /// @dev It is constructed in the globals.
    function deployPBHSignatureAggregator(address _pbhEntryPoint) public {
        pbhAggregator = new PBHSignatureAggregator(_pbhEntryPoint);
    }

    /// @notice Initializes a new safe account.
    /// @dev It is constructed in the globals.
    function deploySafeAndModule(address _pbhSignatureAggregator, uint256 threshold) public {
        pbh4337Module = new Mock4337Module(address(entryPoint), _pbhSignatureAggregator, PBH_NONCE_KEY);

        // Deploy SafeModuleSetup
        moduleSetup = new SafeModuleSetup();

        // Deploy Safe singleton and factory
        singleton = new Safe();
        factory = new SafeProxyFactory();

        // Prepare module initialization
        address[] memory modules = new address[](1);
        modules[0] = address(pbh4337Module);

        // Encode the moduleSetup.enableModules call
        bytes memory moduleSetupCall = abi.encodeCall(SafeModuleSetup.enableModules, (modules));

        // Create owners array with single owner
        address[] memory owners = new address[](1);
        owners[0] = safeOwner;

        // Encode initialization data for proxy
        bytes memory initData = abi.encodeCall(
            Safe.setup,
            (
                owners,
                threshold, // threshold
                address(moduleSetup), // to
                moduleSetupCall, // data
                address(pbh4337Module), // fallbackHandler
                address(0), // paymentToken
                0, // payment
                payable(address(0)) // paymentReceiver
            )
        );

        // Deploy and initialize Safe proxy
        SafeProxy proxy = factory.createProxyWithNonce(
            address(singleton),
            initData,
            0 // salt nonce
        );

        // Cast proxy to Safe for easier interaction
        safe = Safe(payable(address(proxy)));
    }

    /// @notice Initializes a new World ID Groups contract.
    /// @dev It is constructed in the globals.
    function deployWorldIDGroups() public {
        worldIDGroups = new MockWorldIDGroups();
    }

    /// @notice Constructs a new pbhEntryPoint without initializing.
    /// @dev Note that the owner will not be set without initilizing.
    function makeUninitPBHEntryPoint() public {
        pbhEntryPointImpl = address(new PBHEntryPointImplV1());
        pbhEntryPoint = IPBHEntryPoint(address(new PBHEntryPoint(pbhEntryPointImpl, new bytes(0x0))));
    }

    // TODO: remove these

    /// @notice Asserts that making the external call using `callData` on `target` succeeds.
    ///
    /// @param target The target at which to make the call.
    /// @param callData The ABI-encoded call to a function.
    function assertCallSucceedsOn(address target, bytes memory callData) public {
        (bool status,) = target.call(callData);
        assert(status);
    }

    /// @notice Asserts that making the external call using `callData` on `target` succeeds.
    ///
    /// @param target The target at which to make the call.
    /// @param callData The ABI-encoded call to a function.
    /// @param expectedReturnData The expected return data from the function.
    function assertCallSucceedsOn(address target, bytes memory callData, bytes memory expectedReturnData) public {
        (bool status, bytes memory returnData) = target.call(callData);
        assert(status);
        assertEq(expectedReturnData, returnData);
    }

    /// @notice Asserts that making the external call using `callData` on `target` fails.
    ///
    /// @param target The target at which to make the call.
    /// @param callData The ABI-encoded call to a function.
    function assertCallFailsOn(address target, bytes memory callData) public {
        (bool status,) = target.call(callData);
        assert(!status);
    }

    /// @notice Asserts that making the external call using `callData` on `target` fails.
    ///
    /// @param target The target at which to make the call.
    /// @param callData The ABI-encoded call to a function.
    /// @param expectedReturnData The expected return data from the function.
    function assertCallFailsOn(address target, bytes memory callData, bytes memory expectedReturnData) public {
        (bool status, bytes memory returnData) = target.call(callData);
        assert(!status);
        assertEq(expectedReturnData, returnData);
    }

    /// @notice Performs the low-level encoding of the `revert(string)` call's return data.
    /// @dev Equivalent to `abi.encodeWithSignature("Error(string)", reason)`.
    ///
    /// @param reason The string reason for the revert.
    ///
    /// @return data The ABI encoding of the revert.
    function encodeStringRevert(string memory reason) public pure returns (bytes memory data) {
        return abi.encodeWithSignature("Error(string)", reason);
    }
}
