// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {Test, console2} from "forge-std/Test.sol";
import {Deploy} from "../script/Deploy.s.sol";
import {WLDPaymaster} from "../src/WLDPaymaster.sol";
import {IWldEthOracle} from "../src/interfaces/IWldEthOracle.sol";
import {IStakeManager} from "@account-abstraction/interfaces/IStakeManager.sol";
import {PackedUserOperation} from "@account-abstraction/interfaces/PackedUserOperation.sol";
import {IPaymaster} from "@account-abstraction/interfaces/IPaymaster.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/**
 * @notice Runs `script/Deploy.s.sol` against a World Chain fork and proves the
 *         result can actually sponsor a UserOperation. This is the regression
 *         test for the deployment path itself — defaults that don't compose (a
 *         deposit below the floor, an unstaked paymaster, a delay under the
 *         bundler minimum) fail here rather than on mainnet.
 *
 *         Skipped unless `WORLDCHAIN_RPC_URL` is set:
 *
 *         WORLDCHAIN_RPC_URL=https://worldchain-mainnet.g.alchemy.com/public \
 *           forge test --match-path 'test/Deploy.fork.t.sol' -vv
 */
contract DeployForkTest is Test {
    address constant ENTRYPOINT_V07 = 0x0000000071727De22E5E9d8BAf0edAc6f37da032;
    address constant WLD = 0x2cFc85d8E48F8EAB294be644d9E25C3030863003;

    bool skipped;

    function setUp() public {
        string memory rpc = vm.envOr("WORLDCHAIN_RPC_URL", string(""));
        if (bytes(rpc).length == 0) {
            skipped = true;
            return;
        }
        vm.createSelectFork(rpc);
        // The script funds deposit + stake from the broadcasting EOA.
        vm.deal(tx.origin, 10 ether);
    }

    function test_DeployedPaymasterIsReadyToSponsor() public {
        vm.skip(skipped);

        (WLDPaymaster paymaster, IWldEthOracle oracle) = new Deploy().run();

        // --- the script's own guarantees ---
        assertEq(address(paymaster.entryPoint()), ENTRYPOINT_V07, "wired to EntryPoint v0.7");
        assertEq(address(paymaster.oracle()), address(oracle), "oracle wired");
        assertGt(paymaster.getDeposit(), paymaster.minEntryPointDeposit(), "deposit clears its own floor");
        assertGt(paymaster.maxWldPerBatch(), 0, "batch size is bounded");

        IStakeManager.DepositInfo memory info = IStakeManager(ENTRYPOINT_V07).getDepositInfo(address(paymaster));
        assertTrue(info.staked, "staked");
        assertGe(info.unstakeDelaySec, 1 days, "unstake delay clears the bundler minimum");

        // --- and the thing that actually matters: it can sponsor ---
        uint256 maxCost = 0.001 ether;
        address user = makeAddr("user");
        uint256 quote = paymaster.quoteWldCharge(maxCost);
        assertGt(quote, 0, "priced");

        deal(WLD, user, quote * 2);
        vm.prank(user);
        IERC20(WLD).approve(address(paymaster), type(uint256).max);

        PackedUserOperation memory op;
        op.sender = user;

        vm.prank(ENTRYPOINT_V07);
        (bytes memory context, uint256 validationData) = paymaster.validatePaymasterUserOp(op, bytes32(0), maxCost);
        assertEq(validationData, 0, "validation accepted the op");
        assertEq(IERC20(WLD).balanceOf(address(paymaster)), quote, "WLD collected");

        vm.prank(ENTRYPOINT_V07);
        paymaster.postOp(IPaymaster.PostOpMode.opSucceeded, context, 0.0004 ether, 1 gwei);
        assertGt(paymaster.accumulatedWld(), 0, "charge booked for settlement");

        console2.log(
            "sponsorable ops at maxCost 0.001 ETH:",
            (paymaster.getDeposit() - paymaster.minEntryPointDeposit()) / maxCost
        );
    }

    // -------------------------------------------------------------------------
    // Guard tests call `preflight` with an explicit config: env vars are
    // process-global and Foundry runs a suite's tests in parallel, so mutating
    // them with vm.setEnv races across tests.
    // -------------------------------------------------------------------------

    /// @dev The deposit floor is reserved; a deposit at or below it can sponsor
    ///      nothing, and the script must refuse rather than deploy that.
    function test_RevertWhen_DepositDoesNotClearFloor() public {
        vm.skip(skipped);

        Deploy script = new Deploy();
        Deploy.Config memory c = script.config();
        c.deposit = c.minEntryPointDeposit; // equal is not enough

        vm.expectRevert(bytes("DEPOSIT must exceed MIN_ENTRYPOINT_DEPOSIT (floor is reserved)"));
        script.preflight(c);
    }

    function test_RevertWhen_UnstakeDelayBelowBundlerMinimum() public {
        vm.skip(skipped);

        Deploy script = new Deploy();
        Deploy.Config memory c = script.config();
        c.unstakeDelay = 1 hours;

        vm.expectRevert(bytes("UNSTAKE_DELAY below the 1-day bundler minimum"));
        script.preflight(c);
    }

    function test_RevertWhen_NotStaked() public {
        vm.skip(skipped);

        Deploy script = new Deploy();
        Deploy.Config memory c = script.config();
        c.stake = 0;

        vm.expectRevert(bytes("STAKE must be > 0 or bundlers will reject every op"));
        script.preflight(c);
    }

    function test_RevertWhen_NoDeposit() public {
        vm.skip(skipped);

        Deploy script = new Deploy();
        Deploy.Config memory c = script.config();
        c.deposit = 0;

        vm.expectRevert(bytes("DEPOSIT must be > 0 or the paymaster cannot sponsor"));
        script.preflight(c);
    }

    /// @dev Guards against pointing the script at a chain where the deps aren't.
    function test_RevertWhen_DependencyHasNoCode() public {
        vm.skip(skipped);

        Deploy script = new Deploy();
        Deploy.Config memory c = script.config();
        c.router = address(0xdEaD);

        vm.expectRevert(bytes("SWAP_ROUTER has no code on this chain"));
        script.preflight(c);
    }

    /// @dev The deployer must actually be able to fund both deposit and stake.
    function test_RevertWhen_DeployerUnderfunded() public {
        vm.skip(skipped);

        Deploy script = new Deploy();
        Deploy.Config memory c = script.config();
        vm.deal(tx.origin, c.deposit + c.stake - 1);

        vm.expectRevert(bytes("deployer balance < DEPOSIT + STAKE"));
        script.preflight(c);
    }
}
