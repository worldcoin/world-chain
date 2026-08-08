// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {Test, console2} from "forge-std/Test.sol";
import {IEntryPoint} from "@account-abstraction/interfaces/IEntryPoint.sol";
import {PackedUserOperation} from "@account-abstraction/interfaces/PackedUserOperation.sol";
import {IPaymaster} from "@account-abstraction/interfaces/IPaymaster.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

import {WLDPaymaster} from "../src/WLDPaymaster.sol";
import {DeployProxy} from "./utils/DeployProxy.sol";
import {ChainlinkWldEthOracle} from "../src/oracle/ChainlinkWldEthOracle.sol";
import {IAggregatorV3} from "../src/interfaces/IAggregatorV3.sol";
import {IWldEthOracle} from "../src/interfaces/IWldEthOracle.sol";
import {IWETH9, ISwapRouter} from "../src/interfaces/ISwapRouter.sol";

/**
 * @notice End-to-end test against **live World Chain contracts**: real EntryPoint
 *         v0.7, real WLD, real WETH, real SwapRouter02, real WLD/WETH 0.3% pool,
 *         real Chainlink feeds. Nothing is mocked.
 *
 *         This is the test that catches integration breakage the unit tests
 *         cannot — e.g. the SwapRouter02 struct having no `deadline` field.
 *
 *         Skipped unless `WORLDCHAIN_RPC_URL` is set:
 *
 *         WORLDCHAIN_RPC_URL=https://worldchain-mainnet.g.alchemy.com/public \
 *           forge test --match-path 'test/E2E.fork.t.sol' -vv
 */
contract E2EForkTest is Test {
    // --- live World Chain addresses (chain id 480) ---
    address constant ENTRYPOINT_V07 = 0x0000000071727De22E5E9d8BAf0edAc6f37da032;
    address constant WLD = 0x2cFc85d8E48F8EAB294be644d9E25C3030863003;
    address constant WETH = 0x4200000000000000000000000000000000000006;
    /// @dev SwapRouter02 — the legacy `SwapRouter` is NOT deployed on World Chain.
    address constant SWAP_ROUTER_02 = 0x091AD9e2e6e5eD44c1c66dB50e49A601F9f36cF6;
    /// @dev The only WLD/WETH pool with real liquidity (0.3% tier).
    uint24 constant POOL_FEE = 3000;
    address constant WLD_WETH_POOL_3000 = 0x494D68e3cAb640fa50F4c1B3E2499698D1a173A0;
    address constant WLD_USD_FEED = 0x8Bb2943AB030E3eE05a58d9832525B4f60A97FA0;
    address constant ETH_USD_FEED = 0xe1d72a719171DceAB9499757EB9d5AEb9e8D64A6;

    uint256 constant MAX_COST = 0.001 ether;

    WLDPaymaster paymaster;
    address implementation;
    ChainlinkWldEthOracle oracle;
    address user = makeAddr("user");
    bool skipped;

    function setUp() public {
        string memory rpc = vm.envOr("WORLDCHAIN_RPC_URL", string(""));
        if (bytes(rpc).length == 0) {
            skipped = true;
            return;
        }
        vm.createSelectFork(rpc);

        oracle = new ChainlinkWldEthOracle(IAggregatorV3(WLD_USD_FEED), IAggregatorV3(ETH_USD_FEED), 1 hours);
        (paymaster, implementation) = DeployProxy.deploy(
            IEntryPoint(ENTRYPOINT_V07),
            IERC20(WLD),
            IWETH9(WETH),
            ISwapRouter(SWAP_ROUTER_02),
            IWldEthOracle(address(oracle)),
            POOL_FEE,
            address(this)
        );

        vm.deal(address(this), 10 ether);
        paymaster.deposit{value: 1 ether}();

        deal(WLD, user, 10_000e18);
        vm.prank(user);
        IERC20(WLD).approve(address(paymaster), type(uint256).max);
    }

    /// @dev `paymasterAndData` for v0.7: paymaster | verificationGas | postOpGas |
    ///      the client's WLD ceiling.
    function _paymasterAndData(uint256 maxWldAllowed) internal view returns (bytes memory) {
        return abi.encodePacked(address(paymaster), uint128(150_000), uint128(100_000), maxWldAllowed);
    }

    /// @dev Sanity: the addresses baked into the deploy script are what we think.
    function test_LiveAddressesAreAsExpected() public {
        vm.skip(skipped);

        assertGt(ENTRYPOINT_V07.code.length, 0, "EntryPoint v0.7 has code");
        assertEq(IERC20Metadata(WLD).symbol(), "WLD");
        assertEq(IERC20Metadata(WETH).symbol(), "WETH");
        assertEq(IERC20Metadata(WLD).decimals(), 18);
        // SwapRouter02 identifies itself by exposing WETH9()/factory().
        assertEq(ISwapRouter02(SWAP_ROUTER_02).WETH9(), WETH);
        // The 0.3% pool must still hold liquidity or the batch swap can't settle.
        assertGt(IUniswapV3Pool(WLD_WETH_POOL_3000).liquidity(), 0, "pool has liquidity");
    }

    /// @dev The full loop: charge WLD -> reconcile -> swap to ETH -> re-deposit.
    function test_FullLoop_ChargeReconcileSwapRedeposit() public {
        vm.skip(skipped);

        // --- 1. validate: pull the max WLD charge from the user ---
        uint256 quote = paymaster.quoteWldCharge(MAX_COST);
        console2.log("WLD charged for 0.001 ETH of gas (incl +20%):", quote);
        assertGt(quote, 0);

        uint256 userWldBefore = IERC20(WLD).balanceOf(user);
        PackedUserOperation memory op;
        op.sender = user;
        // Client-signed WLD ceiling, quoted with headroom for oracle drift.
        op.paymasterAndData = _paymasterAndData(quote * 2);

        vm.prank(ENTRYPOINT_V07);
        (bytes memory context,) = paymaster.validatePaymasterUserOp(op, bytes32(0), MAX_COST);
        assertEq(userWldBefore - IERC20(WLD).balanceOf(user), quote, "max charge pulled up front");

        // --- 2. postOp: reconcile against actual gas, refund the difference ---
        vm.prank(ENTRYPOINT_V07);
        paymaster.postOp(IPaymaster.PostOpMode.opSucceeded, context, 0.0004 ether, 1 gwei);

        uint256 accumulated = paymaster.accumulatedWld();
        console2.log("WLD kept after pro-rata refund:", accumulated);
        assertGt(accumulated, 0, "charge booked for batching");
        assertLt(accumulated, quote, "user was refunded the unused portion");
        assertEq(
            IERC20(WLD).balanceOf(address(paymaster)), accumulated, "paymaster holds exactly the accumulated amount"
        );

        // --- 3. triggerBatchSwap: real SwapRouter02 + real pool ---
        uint256 depositBefore = paymaster.getDeposit();
        vm.roll(block.number + paymaster.blocksPerBatch());
        assertTrue(paymaster.batchReady(), "batch is ready");

        // Permissionless: crank it from an unrelated address.
        address keeper = makeAddr("keeper");
        vm.prank(keeper);
        uint256 ethOut = paymaster.triggerBatchSwap(0);

        console2.log("ETH out of the batch swap:", ethOut);
        assertGt(ethOut, 0, "swap produced ETH");
        assertEq(paymaster.accumulatedWld(), 0, "accumulator drained");
        assertEq(paymaster.getDeposit(), depositBefore + ethOut, "EntryPoint deposit replenished");
        assertEq(IERC20(WLD).balanceOf(address(paymaster)), 0, "no WLD left behind");
        assertEq(address(paymaster).balance, 0, "no ETH stranded in the paymaster");

        // The swap cleared the oracle-derived floor, i.e. real price impact on the
        // live pool is inside maxSwapSlippageBps for this batch size.
        uint256 fair = oracle.ethForWld(accumulated);
        assertGe(ethOut, (fair * (10_000 - paymaster.maxSwapSlippageBps())) / 10_000, "within slippage bound");
        console2.log("fair ETH value of that WLD:", fair);
    }

    /// @dev The default `maxWldPerBatch` must be swappable on the live pool inside
    ///      the slippage bound — otherwise a full batch stalls settlement forever.
    function test_DefaultBatchCapClearsSlippageOnLivePool() public {
        vm.skip(skipped);

        uint256 cap = paymaster.maxWldPerBatch();
        assertGt(cap, 0, "cap is set by default");

        // Fund + book `cap` WLD directly through the charge path.
        uint256 ethCost = oracle.ethForWld(cap) * 10_000 / (10_000 + paymaster.premiumBps());
        PackedUserOperation memory op;
        op.sender = user;
        op.paymasterAndData = _paymasterAndData(type(uint256).max);
        deal(WLD, user, cap * 2);

        vm.prank(ENTRYPOINT_V07);
        (bytes memory context,) = paymaster.validatePaymasterUserOp(op, bytes32(0), ethCost);
        vm.prank(ENTRYPOINT_V07);
        paymaster.postOp(IPaymaster.PostOpMode.opSucceeded, context, ethCost, 1 gwei);

        uint256 toSell = paymaster.nextBatchAmount();
        console2.log("selling a full default batch (WLD):", toSell);

        vm.roll(block.number + paymaster.blocksPerBatch());
        uint256 ethOut = paymaster.triggerBatchSwap(0);
        console2.log("ETH out:", ethOut);
        assertGt(ethOut, 0, "a full default batch clears the live pool's slippage bound");
    }
}

interface IERC20Metadata {
    function symbol() external view returns (string memory);
    function decimals() external view returns (uint8);
}

interface ISwapRouter02 {
    function WETH9() external view returns (address);
}

interface IUniswapV3Pool {
    function liquidity() external view returns (uint128);
}
