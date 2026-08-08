// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {Test} from "forge-std/Test.sol";
import {EntryPoint} from "@account-abstraction/core/EntryPoint.sol";
import {IEntryPoint} from "@account-abstraction/interfaces/IEntryPoint.sol";
import {IPaymaster} from "@account-abstraction/interfaces/IPaymaster.sol";
import {PackedUserOperation} from "@account-abstraction/interfaces/PackedUserOperation.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";

import {WLDPaymaster} from "../src/WLDPaymaster.sol";
import {IWETH9, ISwapRouter} from "../src/interfaces/ISwapRouter.sol";
import {IWldEthOracle} from "../src/interfaces/IWldEthOracle.sol";
import {MockERC20, MockWETH, MockOracle, MockSwapRouter} from "./mocks/Mocks.sol";
import {DeployProxy} from "./utils/DeployProxy.sol";

/// @dev A trivial v2 that adds state *after* v1's variables — the only layout change
///      that is safe — so the upgrade path can be tested end to end.
contract WLDPaymasterV2 is WLDPaymaster {
    uint256 public newlyAddedField;

    function version() external pure override returns (string memory) {
        return "2.1.0-test";
    }

    /// @dev Migration for the added field, guarded so it can only run once.
    function initializeV2(uint256 _newlyAddedField) external reinitializer(2) {
        newlyAddedField = _newlyAddedField;
    }
}

contract UpgradeabilityTest is Test {
    uint256 constant NUM = 1000;
    uint256 constant DEN = 1;

    EntryPoint entryPoint;
    MockERC20 wld;
    MockWETH weth;
    MockOracle oracle;
    MockSwapRouter router;
    WLDPaymaster paymaster;
    address implementation;

    address owner = address(this);
    address user = makeAddr("user");
    address attacker = makeAddr("attacker");

    uint256 constant MAX_COST = 0.001 ether;

    function setUp() public {
        entryPoint = new EntryPoint();
        wld = new MockERC20("Worldcoin", "WLD");
        weth = new MockWETH();
        oracle = new MockOracle(NUM, DEN);
        router = new MockSwapRouter(weth, NUM, DEN);

        (paymaster, implementation) = DeployProxy.deploy(
            IEntryPoint(address(entryPoint)),
            IERC20(address(wld)),
            IWETH9(address(weth)),
            ISwapRouter(address(router)),
            IWldEthOracle(address(oracle)),
            3000,
            owner
        );

        paymaster.deposit{value: 1 ether}();
        vm.deal(address(router), 100 ether);
        wld.mint(user, 1_000 ether);
        vm.prank(user);
        wld.approve(address(paymaster), type(uint256).max);
    }

    function _implementationOf(address proxy) internal view returns (address) {
        return address(uint160(uint256(vm.load(proxy, ERC1967Utils.IMPLEMENTATION_SLOT))));
    }

    // =====================================================================
    //                          initialization
    // =====================================================================

    function test_ProxyIsInitializedAndPointsAtImplementation() public view {
        assertEq(_implementationOf(address(paymaster)), implementation, "implementation slot");
        assertEq(paymaster.owner(), owner, "owner set by initialize");
        assertEq(address(paymaster.entryPoint()), address(entryPoint));
        assertEq(address(paymaster.wld()), address(wld));
        assertEq(paymaster.premiumBps(), 2_000, "defaults applied through the proxy");
    }

    /// @dev The implementation must be inert: `_disableInitializers()` in its
    ///      constructor means nobody can claim it as their own paymaster.
    function test_RevertWhen_InitializingTheImplementation() public {
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        WLDPaymaster(payable(implementation))
            .initialize(
                IEntryPoint(address(entryPoint)),
                IERC20(address(wld)),
                IWETH9(address(weth)),
                ISwapRouter(address(router)),
                IWldEthOracle(address(oracle)),
                3000,
                attacker
            );
        assertEq(WLDPaymaster(payable(implementation)).owner(), address(0), "implementation is un-owned");
    }

    /// @dev The proxy can only be initialized once — no re-taking ownership.
    function test_RevertWhen_InitializingTheProxyTwice() public {
        vm.prank(attacker);
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        paymaster.initialize(
            IEntryPoint(address(entryPoint)),
            IERC20(address(wld)),
            IWETH9(address(weth)),
            ISwapRouter(address(router)),
            IWldEthOracle(address(oracle)),
            3000,
            attacker
        );
        assertEq(paymaster.owner(), owner, "owner unchanged");
    }

    function test_RevertWhen_InitializedWithZeroAddresses() public {
        WLDPaymaster impl = new WLDPaymaster();
        bytes memory initData = abi.encodeCall(
            WLDPaymaster.initialize,
            (
                IEntryPoint(address(entryPoint)),
                IERC20(address(0)), // wld
                IWETH9(address(weth)),
                ISwapRouter(address(router)),
                IWldEthOracle(address(oracle)),
                3000,
                owner
            )
        );
        vm.expectRevert(WLDPaymaster.InvalidConfig.selector);
        new ERC1967Proxy(address(impl), initData);
    }

    /// @dev A wrong EntryPoint is caught at initialization, not on the first op.
    function test_RevertWhen_EntryPointIsNotAnEntryPoint() public {
        WLDPaymaster impl = new WLDPaymaster();
        bytes memory initData = abi.encodeCall(
            WLDPaymaster.initialize,
            (
                IEntryPoint(address(wld)), // an ERC20, not an EntryPoint
                IERC20(address(wld)),
                IWETH9(address(weth)),
                ISwapRouter(address(router)),
                IWldEthOracle(address(oracle)),
                3000,
                owner
            )
        );
        vm.expectRevert();
        new ERC1967Proxy(address(impl), initData);
    }

    // =====================================================================
    //                          upgrade authorization
    // =====================================================================

    function test_RevertWhen_NonOwnerUpgrades() public {
        address v2 = address(new WLDPaymasterV2());
        vm.prank(attacker);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, attacker));
        paymaster.upgradeToAndCall(v2, "");
        assertEq(_implementationOf(address(paymaster)), implementation, "implementation unchanged");
    }

    /// @dev UUPS: the implementation refuses to be called directly, so an upgrade
    ///      routed at the implementation cannot orphan the proxy.
    function test_RevertWhen_UpgradingTheImplementationDirectly() public {
        address v2 = address(new WLDPaymasterV2());
        vm.expectRevert(); // UUPSUnauthorizedCallContext
        WLDPaymaster(payable(implementation)).upgradeToAndCall(v2, "");
    }

    // =====================================================================
    //                    state survives the upgrade
    // =====================================================================

    function test_UpgradePreservesStateAndFunds() public {
        // Put real state on the proxy: a charge booked and config off its defaults.
        paymaster.setPremiumBps(1_234);
        _chargeOneOp();

        uint256 accumulated = paymaster.accumulatedWld();
        uint256 deposit = paymaster.getDeposit();
        uint256 wldHeld = wld.balanceOf(address(paymaster));
        assertGt(accumulated, 0, "state to preserve");

        address v2 = address(new WLDPaymasterV2());
        paymaster.upgradeToAndCall(v2, abi.encodeCall(WLDPaymasterV2.initializeV2, (42)));

        assertEq(_implementationOf(address(paymaster)), v2, "now on v2");
        assertEq(paymaster.version(), "2.1.0-test", "new logic is live");
        assertEq(WLDPaymasterV2(payable(address(paymaster))).newlyAddedField(), 42, "migration ran");

        // Everything the proxy owned is still there and still attributed correctly.
        assertEq(paymaster.owner(), owner, "owner");
        assertEq(address(paymaster.entryPoint()), address(entryPoint), "entryPoint");
        assertEq(address(paymaster.wld()), address(wld), "wld");
        assertEq(paymaster.premiumBps(), 1_234, "config");
        assertEq(paymaster.accumulatedWld(), accumulated, "booked WLD");
        assertEq(paymaster.getDeposit(), deposit, "EntryPoint deposit");
        assertEq(wld.balanceOf(address(paymaster)), wldHeld, "WLD balance");
    }

    /// @dev The migration is one-shot: a second `initializeV2` cannot rewrite it.
    function test_RevertWhen_ReinitializerRunsTwice() public {
        address v2 = address(new WLDPaymasterV2());
        paymaster.upgradeToAndCall(v2, abi.encodeCall(WLDPaymasterV2.initializeV2, (42)));

        vm.expectRevert(Initializable.InvalidInitialization.selector);
        WLDPaymasterV2(payable(address(paymaster))).initializeV2(43);
        assertEq(WLDPaymasterV2(payable(address(paymaster))).newlyAddedField(), 42);
    }

    /// @dev The proxy address is what users approved and what clients encode, so it
    ///      must keep sponsoring after an upgrade with no client-side change.
    function test_SponsorsAfterUpgrade() public {
        paymaster.upgradeToAndCall(address(new WLDPaymasterV2()), "");
        uint256 booked = paymaster.accumulatedWld();
        _chargeOneOp();
        assertGt(paymaster.accumulatedWld(), booked, "charged an op on v2");
    }

    /// @dev Ownership can move to a multisig, and only the new owner may upgrade.
    function test_UpgradeAuthorityFollowsOwnership() public {
        address multisig = makeAddr("multisig");
        paymaster.transferOwnership(multisig);

        address v2 = address(new WLDPaymasterV2());
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, owner));
        paymaster.upgradeToAndCall(v2, "");

        vm.prank(multisig);
        paymaster.upgradeToAndCall(v2, "");
        assertEq(_implementationOf(address(paymaster)), v2, "new owner upgraded");
    }

    // =====================================================================

    /// @dev Drives the real charge path so the upgrade tests have live state.
    function _chargeOneOp() internal {
        PackedUserOperation memory op;
        op.sender = user;
        op.paymasterAndData = abi.encodePacked(address(paymaster), uint128(150_000), uint128(100_000));

        vm.prank(address(entryPoint));
        (bytes memory context,) = paymaster.validatePaymasterUserOp(op, bytes32(0), MAX_COST);
        vm.prank(address(entryPoint));
        paymaster.postOp(IPaymaster.PostOpMode.opSucceeded, context, 0.0004 ether, 1 gwei);
    }

    receive() external payable {}
}
