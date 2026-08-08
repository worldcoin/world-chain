// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {Test, console2} from "forge-std/Test.sol";
import {EntryPoint} from "@account-abstraction/core/EntryPoint.sol";
import {IEntryPoint} from "@account-abstraction/interfaces/IEntryPoint.sol";
import {PackedUserOperation} from "@account-abstraction/interfaces/PackedUserOperation.sol";

import {WLDPaymaster} from "../src/WLDPaymaster.sol";
import {DeployProxy} from "./utils/DeployProxy.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IWETH9, ISwapRouter} from "../src/interfaces/ISwapRouter.sol";
import {IWldEthOracle} from "../src/interfaces/IWldEthOracle.sol";
import {MockERC20, MockWETH, MockOracle, MockSwapRouter, MockAccount} from "./mocks/Mocks.sol";

/**
 * @notice Drives a real `EntryPoint.handleOps` end to end, rather than pranking the
 *         EntryPoint and calling `validatePaymasterUserOp` directly.
 *
 * @dev This distinction matters. `EntryPoint._validatePaymasterPrepayment` does:
 *
 *          paymasterInfo.deposit = deposit - requiredPreFund;   // FIRST
 *          try IPaymaster(paymaster).validatePaymasterUserOp(...)
 *
 *      so inside validation `getDeposit()` is already **post-prefund**. Tests that
 *      call validation directly never see that deduction, which is exactly how a
 *      double-counted deposit floor slipped through: the floor check compared the
 *      post-prefund balance against `maxCost + minEntryPointDeposit`, demanding
 *      ~`2 * maxCost + floor` and rejecting ops that left the floor fully intact.
 */
contract EntryPointIntegrationTest is Test {
    uint256 constant NUM = 1000; // 1 ETH = 1000 WLD
    uint256 constant DEN = 1;

    EntryPoint entryPoint;
    MockERC20 wld;
    MockWETH weth;
    MockOracle oracle;
    MockSwapRouter router;
    WLDPaymaster paymaster;
    address implementation;
    MockAccount account;

    address beneficiary = makeAddr("beneficiary");

    // Gas limits kept small and explicit so `maxCost` is easy to reason about.
    uint128 constant VERIFICATION_GAS = 150_000;
    uint128 constant CALL_GAS = 50_000;
    uint128 constant PM_VERIFICATION_GAS = 150_000;
    uint128 constant PM_POSTOP_GAS = 100_000;
    uint256 constant PRE_VERIFICATION_GAS = 50_000;
    uint256 constant MAX_FEE_PER_GAS = 1 gwei;

    function setUp() public {
        entryPoint = new EntryPoint();
        wld = new MockERC20("Worldcoin", "WLD");
        weth = new MockWETH();
        oracle = new MockOracle(NUM, DEN);
        router = new MockSwapRouter(weth, NUM, DEN);
        account = new MockAccount();

        (paymaster, implementation) = DeployProxy.deploy(
            IEntryPoint(address(entryPoint)),
            IERC20(address(wld)),
            IWETH9(address(weth)),
            ISwapRouter(address(router)),
            IWldEthOracle(address(oracle)),
            3000,
            address(this)
        );

        vm.deal(address(router), 100 ether);
        wld.mint(address(account), 1_000_000 ether);
        vm.prank(address(account));
        wld.approve(address(paymaster), type(uint256).max);
    }

    /// @dev `maxCost` as the EntryPoint computes it: all gas limits × maxFeePerGas.
    function _maxCost() internal pure returns (uint256) {
        return (uint256(VERIFICATION_GAS) + CALL_GAS + PM_VERIFICATION_GAS + PM_POSTOP_GAS + PRE_VERIFICATION_GAS)
            * MAX_FEE_PER_GAS;
    }

    function _buildOp() internal view returns (PackedUserOperation memory op) {
        op.sender = address(account);
        op.nonce = entryPoint.getNonce(address(account), 0);
        op.accountGasLimits = bytes32((uint256(VERIFICATION_GAS) << 128) | uint256(CALL_GAS));
        op.preVerificationGas = PRE_VERIFICATION_GAS;
        op.gasFees = bytes32((uint256(MAX_FEE_PER_GAS) << 128) | uint256(MAX_FEE_PER_GAS));
        // Client-signed WLD ceiling, generous so these tests exercise other paths.
        op.paymasterAndData =
            abi.encodePacked(address(paymaster), PM_VERIFICATION_GAS, PM_POSTOP_GAS, type(uint256).max);
    }

    function _handleOps() internal {
        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = _buildOp();
        entryPoint.handleOps(ops, payable(beneficiary));
    }

    // =========================================================================
    //                       deposit floor semantics
    // =========================================================================

    /// @dev The floor means "this much must REMAIN after sponsoring the op". A
    ///      deposit of exactly `maxCost + floor` satisfies that and must be
    ///      accepted. Before the fix this reverted with DepositFloorBreached
    ///      because the already-deducted `maxCost` was counted a second time.
    function test_FloorIsNotDoubleCounted() public {
        uint256 maxCost = _maxCost();
        uint256 floor = 0.01 ether;
        paymaster.setMinEntryPointDeposit(floor);
        paymaster.deposit{value: maxCost + floor}();

        _handleOps(); // must not revert

        // The floor survived: only actual gas (<= maxCost) was consumed.
        assertGe(paymaster.getDeposit(), floor, "floor intact after sponsoring");
        assertGt(paymaster.accumulatedWld(), 0, "op was actually sponsored");
    }

    /// @dev One wei below `maxCost + floor` must still be refused.
    function test_RevertWhen_DepositOneWeiBelowFloorPlusMaxCost() public {
        uint256 maxCost = _maxCost();
        uint256 floor = 0.01 ether;
        paymaster.setMinEntryPointDeposit(floor);
        paymaster.deposit{value: maxCost + floor - 1}();

        // Build first: _buildOp() calls entryPoint.getNonce, which would otherwise
        // consume the expectRevert.
        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = _buildOp();

        vm.expectRevert(
            abi.encodeWithSelector(
                IEntryPoint.FailedOpWithRevert.selector,
                0,
                "AA33 reverted",
                abi.encodeWithSelector(WLDPaymaster.DepositFloorBreached.selector)
            )
        );
        entryPoint.handleOps(ops, payable(beneficiary));
    }

    /// @dev A zero floor should let the deposit be spent down to bare `maxCost`.
    function test_ZeroFloorAllowsFullSpend() public {
        uint256 maxCost = _maxCost();
        paymaster.setMinEntryPointDeposit(0);
        paymaster.deposit{value: maxCost}();

        _handleOps();
        assertGt(paymaster.accumulatedWld(), 0, "sponsored with no floor headroom");
    }

    // =========================================================================
    //                     the charge path, for real
    // =========================================================================

    /// @dev postOp only runs because `paymasterPostOpGasLimit` is non-zero; this
    ///      is the refund path the README warns clients about.
    function test_ChargeAndRefundThroughEntryPoint() public {
        paymaster.setMinEntryPointDeposit(0.01 ether);
        paymaster.deposit{value: 1 ether}();

        uint256 maxCharge = paymaster.quoteWldCharge(_maxCost());
        uint256 wldBefore = wld.balanceOf(address(account));

        _handleOps();

        uint256 spent = wldBefore - wld.balanceOf(address(account));
        assertGt(spent, 0, "user paid WLD for gas");
        assertLt(spent, maxCharge, "user was refunded the unused portion");
        assertEq(paymaster.accumulatedWld(), spent, "charge booked for settlement");
        assertEq(wld.balanceOf(address(paymaster)), spent, "paymaster holds exactly that");

        console2.log("max WLD charge:", maxCharge);
        console2.log("actual charged:", spent);
    }

    /// @dev With a zero postOp gas limit v0.7 skips postOp entirely: the user eats
    ///      the full max charge and nothing is booked. Documented client footgun.
    function test_ZeroPostOpGasLimit_SkipsRefund() public {
        paymaster.setMinEntryPointDeposit(0.01 ether);
        paymaster.deposit{value: 1 ether}();

        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = _buildOp();
        ops[0].paymasterAndData =
            abi.encodePacked(address(paymaster), PM_VERIFICATION_GAS, uint128(0), type(uint256).max);

        uint256 wldBefore = wld.balanceOf(address(account));
        entryPoint.handleOps(ops, payable(beneficiary));

        uint256 spent = wldBefore - wld.balanceOf(address(account));
        assertGt(spent, 0, "user was charged");
        assertEq(paymaster.accumulatedWld(), 0, "postOp never ran: nothing booked");
        assertEq(wld.balanceOf(address(paymaster)), spent, "WLD sits unbooked, recoverable via sweep");
    }

    /// @dev Ops keep flowing until the deposit reaches the floor, then stop.
    function test_SponsorsUntilFloorThenRejects() public {
        uint256 maxCost = _maxCost();
        uint256 floor = 0.01 ether;
        paymaster.setMinEntryPointDeposit(floor);
        paymaster.deposit{value: floor + maxCost * 3}();

        uint256 sponsored;
        uint256 attempts = 200;
        for (uint256 i = 0; i < attempts; i++) {
            try this.handleOpsExternal() {
                sponsored++;
            } catch {
                break;
            }
        }

        assertGt(sponsored, 0, "sponsored at least one op");
        assertLt(sponsored, attempts, "the floor eventually stopped sponsoring");
        assertGe(paymaster.getDeposit(), floor, "never dipped below the floor");
        console2.log("ops sponsored before hitting the floor:", sponsored);
    }

    /// @dev The ceiling is enforced through the real EntryPoint too: a too-tight
    ///      cap surfaces as AA33 and nothing is pulled from the user.
    function test_RevertWhen_ClientMaxWldTooLow() public {
        paymaster.setMinEntryPointDeposit(0.01 ether);
        paymaster.deposit{value: 1 ether}();

        uint256 maxCharge = paymaster.quoteWldCharge(_maxCost());
        uint256 wldBefore = wld.balanceOf(address(account));

        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = _buildOp();
        ops[0].paymasterAndData =
            abi.encodePacked(address(paymaster), PM_VERIFICATION_GAS, PM_POSTOP_GAS, maxCharge - 1);

        vm.expectRevert(
            abi.encodeWithSelector(
                IEntryPoint.FailedOpWithRevert.selector,
                0,
                "AA33 reverted",
                abi.encodeWithSelector(WLDPaymaster.WldChargeExceedsMax.selector, maxCharge, maxCharge - 1)
            )
        );
        entryPoint.handleOps(ops, payable(beneficiary));

        assertEq(wld.balanceOf(address(account)), wldBefore, "no WLD pulled");
    }

    function handleOpsExternal() external {
        _handleOps();
    }

    receive() external payable {}
}
