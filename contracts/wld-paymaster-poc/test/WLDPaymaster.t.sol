// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {Test} from "forge-std/Test.sol";
import {EntryPoint} from "@account-abstraction/core/EntryPoint.sol";
import {IEntryPoint} from "@account-abstraction/interfaces/IEntryPoint.sol";
import {PackedUserOperation} from "@account-abstraction/interfaces/PackedUserOperation.sol";
import {IPaymaster} from "@account-abstraction/interfaces/IPaymaster.sol";

import {WLDPaymaster} from "../src/WLDPaymaster.sol";
import {DeployProxy} from "./utils/DeployProxy.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IWETH9, ISwapRouter} from "../src/interfaces/ISwapRouter.sol";
import {IWldEthOracle} from "../src/interfaces/IWldEthOracle.sol";
import {MockERC20, MockWETH, MockOracle, MockSwapRouter} from "./mocks/Mocks.sol";

contract WLDPaymasterTest is Test {
    // 1 ETH = 1000 WLD
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

    uint256 constant MAX_COST = 0.001 ether; // 1e15 wei

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
            address(this)
        );

        // Fund the paymaster's EntryPoint deposit once.
        paymaster.deposit{value: 1 ether}();

        // Fund the router with ETH so it can back WETH minting.
        vm.deal(address(router), 100 ether);

        // Give the user WLD and approve the paymaster.
        wld.mint(user, 1_000 ether);
        vm.prank(user);
        wld.approve(address(paymaster), type(uint256).max);
    }

    // --- helpers ---

    /// @dev Default: no ceiling, so tests that don't care about the cap behave as
    ///      before. Cap-specific tests pass their own.
    function _userOp(address sender) internal view returns (PackedUserOperation memory op) {
        return _userOp(sender, type(uint256).max);
    }

    function _userOp(address sender, uint256 maxWldAllowed) internal view returns (PackedUserOperation memory op) {
        op.sender = sender;
        op.paymasterAndData = abi.encodePacked(address(paymaster), uint128(150_000), uint128(100_000), maxWldAllowed);
    }

    function _validate(uint256 maxCost) internal returns (bytes memory context) {
        vm.prank(address(entryPoint));
        (context,) = paymaster.validatePaymasterUserOp(_userOp(user), bytes32(0), maxCost);
    }

    function _postOp(bytes memory context, uint256 actualGasCost, uint256 feePerGas) internal {
        vm.prank(address(entryPoint));
        paymaster.postOp(IPaymaster.PostOpMode.opSucceeded, context, actualGasCost, feePerGas);
    }

    // =====================================================================
    //                          Premium math
    // =====================================================================

    function test_QuoteAppliesPremium() public view {
        // base = MAX_COST * 1000 = 1e18 ; +20% => 1.2e18
        uint256 expectedBase = MAX_COST * NUM / DEN;
        assertEq(expectedBase, 1e18);
        assertEq(paymaster.quoteWldCharge(MAX_COST), 1.2e18);
    }

    // =====================================================================
    //                    validate + postOp charge flow
    // =====================================================================

    function test_Validate_PullsMaxWld() public {
        uint256 before = wld.balanceOf(address(paymaster));
        _validate(MAX_COST);
        uint256 pulled = wld.balanceOf(address(paymaster)) - before;
        assertEq(pulled, 1.2e18, "should pull max charge incl premium");
    }

    function test_PostOp_RefundsProRata() public {
        bytes memory ctx = _validate(MAX_COST);

        uint256 userBalAfterValidate = wld.balanceOf(user);

        uint256 actualGasCost = 0.0004 ether; // 4e14
        uint256 feePerGas = 1 gwei; // 1e9
        _postOp(ctx, actualGasCost, feePerGas);

        // costWithPostOp = 4e14 + 40000*1e9 = 4.4e14 ; charge = 1.2e18 * 4.4e14/1e15
        uint256 expectedCharge = 1.2e18 * (actualGasCost + paymaster.postOpGasOverhead() * feePerGas) / MAX_COST;
        assertEq(paymaster.accumulatedWld(), expectedCharge, "accumulated == actual charge");

        uint256 refund = 1.2e18 - expectedCharge;
        assertEq(wld.balanceOf(user) - userBalAfterValidate, refund, "user refunded the difference");
    }

    function test_PostOp_CapsAtWldTaken() public {
        bytes memory ctx = _validate(MAX_COST);
        // actualGasCost already == maxCost; overhead pushes above -> capped
        _postOp(ctx, MAX_COST, 1 gwei);
        assertEq(paymaster.accumulatedWld(), 1.2e18, "charge capped at max WLD");
        assertEq(wld.balanceOf(address(paymaster)), 1.2e18);
    }

    /// @dev postOp must settle at the rate frozen during validation. If it re-read
    ///      the oracle, a mid-op price move would change what the user pays.
    function test_PostOp_UsesRateFromContext_NotFreshOracle() public {
        bytes memory ctx = _validate(MAX_COST);
        uint256 userBalAfterValidate = wld.balanceOf(user);

        uint256 actualGasCost = 0.0004 ether;
        uint256 feePerGas = 1 gwei;
        uint256 expectedCharge = 1.2e18 * (actualGasCost + paymaster.postOpGasOverhead() * feePerGas) / MAX_COST;

        // WLD halves against ETH between validation and settlement.
        oracle.setRate(NUM * 2, DEN);
        _postOp(ctx, actualGasCost, feePerGas);

        assertEq(paymaster.accumulatedWld(), expectedCharge, "charged at the frozen rate");
        assertEq(wld.balanceOf(user) - userBalAfterValidate, 1.2e18 - expectedCharge, "refund too");
    }

    // =====================================================================
    //                    client-supplied WLD ceiling
    // =====================================================================

    /// @dev The ceiling is what the user signed off on: a charge above it must
    ///      abort the op rather than pull the WLD anyway.
    function test_RevertWhen_ChargeExceedsClientMax() public {
        uint256 quote = paymaster.quoteWldCharge(MAX_COST); // 1.2e18
        vm.prank(address(entryPoint));
        vm.expectRevert(abi.encodeWithSelector(WLDPaymaster.WldChargeExceedsMax.selector, quote, quote - 1));
        paymaster.validatePaymasterUserOp(_userOp(user, quote - 1), bytes32(0), MAX_COST);
    }

    /// @dev Exactly at the ceiling is allowed.
    function test_ChargeExactlyAtClientMaxIsAccepted() public {
        uint256 quote = paymaster.quoteWldCharge(MAX_COST);
        vm.prank(address(entryPoint));
        paymaster.validatePaymasterUserOp(_userOp(user, quote), bytes32(0), MAX_COST);
        assertEq(wld.balanceOf(address(paymaster)), quote, "pulled exactly the ceiling");
    }

    /// @dev A premium raised between quote and inclusion cannot overcharge.
    function test_RevertWhen_PremiumRaisedAfterQuote() public {
        uint256 quote = paymaster.quoteWldCharge(MAX_COST);
        paymaster.setPremiumBps(5_000); // +50%, above the quoted +20%
        uint256 raised = paymaster.quoteWldCharge(MAX_COST);

        vm.prank(address(entryPoint));
        vm.expectRevert(abi.encodeWithSelector(WLDPaymaster.WldChargeExceedsMax.selector, raised, quote));
        paymaster.validatePaymasterUserOp(_userOp(user, quote), bytes32(0), MAX_COST);
        assertEq(wld.balanceOf(address(paymaster)), 0, "no WLD pulled");
    }

    /// @dev An explicit 0 opts out of the cap: the charge goes through unbounded.
    function test_ZeroClientMaxDisablesTheCheck() public {
        uint256 quote = paymaster.quoteWldCharge(MAX_COST);

        vm.prank(address(entryPoint));
        paymaster.validatePaymasterUserOp(_userOp(user, 0), bytes32(0), MAX_COST);
        assertEq(wld.balanceOf(address(paymaster)), quote, "charged with no ceiling");
    }

    /// @dev 0 is an opt-out, not a zero-tolerance cap: even a premium raised after
    ///      the quote is accepted.
    function test_ZeroClientMax_AcceptsRaisedPremium() public {
        paymaster.setPremiumBps(5_000);
        uint256 raised = paymaster.quoteWldCharge(MAX_COST);

        vm.prank(address(entryPoint));
        paymaster.validatePaymasterUserOp(_userOp(user, 0), bytes32(0), MAX_COST);
        assertEq(wld.balanceOf(address(paymaster)), raised, "no ceiling to breach");
    }

    /// @dev The ceiling is optional: omitting it entirely means no cap, same as 0.
    ///      The gas-limit bytes before it belong to the EntryPoint, not to us.
    function test_OmittedPaymasterDataMeansNoCeiling() public {
        uint256 quote = paymaster.quoteWldCharge(MAX_COST);

        PackedUserOperation memory op;
        op.sender = user;
        op.paymasterAndData = abi.encodePacked(address(paymaster), uint128(150_000), uint128(100_000));

        vm.prank(address(entryPoint));
        paymaster.validatePaymasterUserOp(op, bytes32(0), MAX_COST);
        assertEq(wld.balanceOf(address(paymaster)), quote, "charged with the field omitted");
    }

    /// @dev A wrong-length payload is a client bug, not something to reinterpret.
    function test_RevertWhen_PaymasterDataWrongLength() public {
        PackedUserOperation memory op;
        op.sender = user;
        op.paymasterAndData = abi.encodePacked(address(paymaster), uint128(150_000), uint128(100_000), uint64(1e18));

        vm.prank(address(entryPoint));
        vm.expectRevert(abi.encodeWithSelector(WLDPaymaster.InvalidPaymasterData.selector, 8));
        paymaster.validatePaymasterUserOp(op, bytes32(0), MAX_COST);
    }

    function test_EncodePaymasterAndData_RoundTrips() public {
        bytes memory pd = paymaster.encodePaymasterAndData(150_000, 100_000, 5e18);
        assertEq(pd.length, 52 + 32, "packed layout with the ceiling");
        assertEq(paymaster.encodePaymasterAndData(150_000, 100_000, 0).length, 52, "0 omits the ceiling");

        PackedUserOperation memory op;
        op.sender = user;
        op.paymasterAndData = pd;
        vm.prank(address(entryPoint));
        paymaster.validatePaymasterUserOp(op, bytes32(0), MAX_COST); // 1.2e18 <= 5e18
    }

    // =====================================================================
    //                          Revert / edge cases
    // =====================================================================

    function test_RevertWhen_InsufficientWldBalance() public {
        address poor = makeAddr("poor");
        vm.prank(poor);
        wld.approve(address(paymaster), type(uint256).max);
        vm.prank(address(entryPoint));
        vm.expectRevert(WLDPaymaster.InsufficientWldBalance.selector);
        paymaster.validatePaymasterUserOp(_userOp(poor), bytes32(0), MAX_COST);
    }

    function test_RevertWhen_InsufficientAllowance() public {
        vm.prank(user);
        wld.approve(address(paymaster), 0);
        vm.prank(address(entryPoint));
        vm.expectRevert(WLDPaymaster.InsufficientWldAllowance.selector);
        paymaster.validatePaymasterUserOp(_userOp(user), bytes32(0), MAX_COST);
    }

    function test_RevertWhen_StaleOracle() public {
        oracle.setStale(true);
        vm.prank(address(entryPoint));
        vm.expectRevert(bytes("OLD"));
        paymaster.validatePaymasterUserOp(_userOp(user), bytes32(0), MAX_COST);
    }

    /// @dev `getDeposit()` is already net of this op's prefund when the real
    ///      EntryPoint calls validation, so the floor check compares the remaining
    ///      balance against the floor directly. These direct-prank tests skip that
    ///      deduction, so the floor is exercised by raising it above the deposit.
    ///      The ordering-faithful version lives in test/EntryPointIntegration.t.sol.
    function test_RevertWhen_DepositFloorBreached() public {
        paymaster.setMinEntryPointDeposit(paymaster.getDeposit() + 1);
        vm.prank(address(entryPoint));
        vm.expectRevert(WLDPaymaster.DepositFloorBreached.selector);
        paymaster.validatePaymasterUserOp(_userOp(user), bytes32(0), MAX_COST);
    }

    /// @dev A deposit exactly at the floor is acceptable: the floor is a minimum to
    ///      retain, not a strict inequality against the remaining balance.
    function test_DepositExactlyAtFloorIsAccepted() public {
        paymaster.setMinEntryPointDeposit(paymaster.getDeposit());
        vm.prank(address(entryPoint));
        paymaster.validatePaymasterUserOp(_userOp(user), bytes32(0), MAX_COST);
    }

    function test_RevertWhen_OnlyEntryPointCanValidate() public {
        vm.expectRevert(bytes("Sender not EntryPoint"));
        paymaster.validatePaymasterUserOp(_userOp(user), bytes32(0), MAX_COST);
    }

    function test_RevertWhen_OnlyEntryPointCanPostOp() public {
        vm.expectRevert(bytes("Sender not EntryPoint"));
        paymaster.postOp(IPaymaster.PostOpMode.opSucceeded, "", 0, 0);
    }

    // =====================================================================
    //                          Batch swap
    // =====================================================================

    function _accumulate() internal returns (uint256 charged) {
        bytes memory ctx = _validate(MAX_COST);
        _postOp(ctx, 0.0004 ether, 1 gwei);
        charged = paymaster.accumulatedWld();
    }

    function test_RevertWhen_BatchTooEarly() public {
        _accumulate();
        vm.expectRevert(WLDPaymaster.BatchTooEarly.selector);
        paymaster.triggerBatchSwap(0);
    }

    function test_RevertWhen_NothingToSwap() public {
        vm.roll(block.number + paymaster.blocksPerBatch());
        vm.expectRevert(WLDPaymaster.NothingToSwap.selector);
        paymaster.triggerBatchSwap(0);
    }

    function test_BatchSwap_ReplenishesEntryPoint() public {
        uint256 charged = _accumulate();
        assertGt(charged, 0);

        uint256 depositBefore = paymaster.getDeposit();

        vm.roll(block.number + paymaster.blocksPerBatch());
        uint256 ethOut = paymaster.triggerBatchSwap(0);

        // ETH out = charged WLD / 1000 (no router slippage)
        assertEq(ethOut, charged * DEN / NUM, "eth out at oracle price");
        assertEq(paymaster.accumulatedWld(), 0, "accumulator reset");
        assertEq(paymaster.lastBatchBlock(), block.number, "batch block updated");
        assertEq(paymaster.getDeposit(), depositBefore + ethOut, "deposit replenished");
    }

    function test_BatchSwap_SlippageProtection() public {
        _accumulate();
        // Router executes 5% worse than oracle; slippage tolerance is 3% -> revert.
        router.setSlippageBps(500);
        vm.roll(block.number + paymaster.blocksPerBatch());
        vm.expectRevert(bytes("Too little received"));
        paymaster.triggerBatchSwap(0);
    }

    function test_BatchSwap_KeeperReward() public {
        paymaster.setKeeperRewardBps(100); // 1%
        uint256 charged = _accumulate();

        address keeper = makeAddr("keeper");
        uint256 keeperEthBefore = keeper.balance;

        vm.roll(block.number + paymaster.blocksPerBatch());
        vm.prank(keeper);
        uint256 ethOut = paymaster.triggerBatchSwap(0);

        uint256 expectedReward = ethOut * 100 / 10_000;
        assertEq(keeper.balance - keeperEthBefore, expectedReward, "keeper paid reward");
        assertEq(ethOut, charged * DEN / NUM);
    }

    function test_BatchReadyView() public {
        assertFalse(paymaster.batchReady());
        _accumulate();
        assertFalse(paymaster.batchReady(), "not enough blocks yet");
        vm.roll(block.number + paymaster.blocksPerBatch());
        assertTrue(paymaster.batchReady());
    }

    // =====================================================================
    //                          Owner config
    // =====================================================================

    function test_OwnerCanConfigure() public {
        paymaster.setPremiumBps(1000);
        assertEq(paymaster.premiumBps(), 1000);
        paymaster.setBlocksPerBatch(10);
        assertEq(paymaster.blocksPerBatch(), 10);
        paymaster.setMaxSwapSlippageBps(150);
        assertEq(paymaster.maxSwapSlippageBps(), 150);
    }

    function test_RevertWhen_NonOwnerConfigures() public {
        vm.prank(user);
        vm.expectRevert();
        paymaster.setPremiumBps(1000);
    }

    function test_RevertWhen_PremiumTooHigh() public {
        vm.expectRevert(WLDPaymaster.InvalidConfig.selector);
        paymaster.setPremiumBps(10_001);
    }

    // =====================================================================
    //                       Batch size cap
    // =====================================================================

    function test_NextBatchAmount_AppliesCap() public {
        uint256 charged = _accumulate();
        assertEq(paymaster.nextBatchAmount(), charged, "uncapped below the limit");

        paymaster.setMaxWldPerBatch(charged / 4);
        assertEq(paymaster.nextBatchAmount(), charged / 4, "capped");

        paymaster.setMaxWldPerBatch(0);
        assertEq(paymaster.nextBatchAmount(), charged, "0 = unlimited");
    }

    /// @dev A backlog larger than the cap must swap a slice and keep the rest,
    ///      not revert forever on price impact.
    function test_BatchSwap_SwapsOnlyUpToCap() public {
        uint256 charged = _accumulate();
        uint256 cap = charged / 4;
        paymaster.setMaxWldPerBatch(cap);

        vm.roll(block.number + paymaster.blocksPerBatch());
        uint256 ethOut = paymaster.triggerBatchSwap(0);

        assertEq(ethOut, cap * DEN / NUM, "only the capped slice was sold");
        assertEq(paymaster.accumulatedWld(), charged - cap, "remainder still accumulated");
    }

    function test_BatchSwap_DrainsBacklogOverBatches() public {
        uint256 charged = _accumulate();
        paymaster.setMaxWldPerBatch(charged / 2 + 1);

        vm.roll(block.number + paymaster.blocksPerBatch());
        paymaster.triggerBatchSwap(0);
        assertGt(paymaster.accumulatedWld(), 0, "one batch is not enough");

        vm.roll(block.number + paymaster.blocksPerBatch());
        paymaster.triggerBatchSwap(0);
        assertEq(paymaster.accumulatedWld(), 0, "drained on the second batch");
    }

    /// @dev The cap must not leak WLD: everything sold or still accumulated.
    function test_BatchSwap_CapConservesWld() public {
        uint256 charged = _accumulate();
        paymaster.setMaxWldPerBatch(charged / 3);
        uint256 balBefore = wld.balanceOf(address(paymaster));

        vm.roll(block.number + paymaster.blocksPerBatch());
        paymaster.triggerBatchSwap(0);

        uint256 sold = balBefore - wld.balanceOf(address(paymaster));
        assertEq(sold + paymaster.accumulatedWld(), charged, "no WLD unaccounted for");
    }

    receive() external payable {}
}
