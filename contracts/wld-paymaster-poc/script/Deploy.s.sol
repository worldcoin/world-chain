// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {Script, console2} from "forge-std/Script.sol";
import {WLDPaymaster} from "../src/WLDPaymaster.sol";
import {ChainlinkWldEthOracle} from "../src/oracle/ChainlinkWldEthOracle.sol";
import {UniswapV3TwapOracle} from "../src/oracle/UniswapV3TwapOracle.sol";
import {IEntryPoint} from "@account-abstraction/interfaces/IEntryPoint.sol";
import {IStakeManager} from "@account-abstraction/interfaces/IStakeManager.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IWETH9, ISwapRouter} from "../src/interfaces/ISwapRouter.sol";
import {IWldEthOracle} from "../src/interfaces/IWldEthOracle.sol";
import {IAggregatorV3} from "../src/interfaces/IAggregatorV3.sol";

/**
 * @notice Deploys and fully configures the WLD paymaster so it is **ready to
 *         sponsor** ERC-4337 v0.7 UserOperations, then asserts readiness before
 *         returning. Any unmet precondition aborts the run rather than leaving a
 *         half-configured paymaster on-chain.
 *
 * What it does, in order:
 *   1. Pre-flight: every configured address must have code; the deployer must hold
 *      DEPOSIT + STAKE; the oracle must return a live price *before* the paymaster
 *      is deployed.
 *   2. Deploy the oracle (Chainlink by default) and the paymaster.
 *   3. Apply all owner configuration in one pass.
 *   4. `deposit` (gas the paymaster spends) and `addStake` (required by bundler
 *      storage rules — see below).
 *   5. Post-flight: assert the deposit clears the paymaster's own floor, the stake
 *      and unstake delay clear bundler minimums, and pricing works end to end.
 *
 * Usage (World Chain mainnet defaults; drop `--broadcast` for a dry run):
 *
 *   export WORLDCHAIN_RPC_URL=https://worldchain-mainnet.g.alchemy.com/public
 *   forge script script/Deploy.s.sol:Deploy \
 *     --rpc-url "$WORLDCHAIN_RPC_URL" --broadcast -vvv
 *
 * Every value below has a working default; override only what you need.
 *
 * Funding (both come from the broadcasting EOA):
 *   DEPOSIT          - wei deposited to the EntryPoint to pay for gas (default 0.02e18).
 *                      Must exceed MIN_ENTRYPOINT_DEPOSIT, which is reserved and
 *                      not spendable on ops.
 *   STAKE            - wei staked on the EntryPoint (default 0.05e18). The EntryPoint
 *                      enforces no minimum; bundlers do, and it is their config.
 *   UNSTAKE_DELAY    - stake unlock delay, seconds (default 86400 = 1 day, the
 *                      ERC-4337 canonical-mempool minimum; bundlers reject less)
 *
 * Addresses (default to live World Chain mainnet, chain id 480):
 *   ENTRYPOINT, WLD, WETH, SWAP_ROUTER, POOL_FEE
 *
 * Pricing:
 *   ORACLE_KIND=chainlink (default) - WLD_USD_FEED, ETH_USD_FEED, MAX_STALENESS
 *   ORACLE_KIND=twap                - WLD_WETH_POOL, TWAP_WINDOW
 *
 * Paymaster policy (all optional; contract defaults used when unset):
 *   PREMIUM_BPS, BLOCKS_PER_BATCH, MAX_SWAP_SLIPPAGE_BPS, MAX_WLD_PER_BATCH,
 *   MIN_ENTRYPOINT_DEPOSIT, POSTOP_GAS_OVERHEAD, KEEPER_REWARD_BPS
 *
 * Ownership:
 *   OWNER            - transfer ownership here after configuring (recommended: a
 *                      multisig). The owner can drain the deposit and replace the
 *                      price oracle, so do not leave a hot EOA in charge.
 *
 * STAKING IS MANDATORY, not a nicety: `validatePaymasterUserOp` writes the
 * paymaster's own associated storage in the WLD contract, which ERC-7562 permits
 * only for a staked entity. Unstaked, its ops are rejected by every compliant
 * bundler regardless of sidecar whitelisting.
 */
contract Deploy is Script {
    // --- live World Chain mainnet (chain id 480) ---
    address constant DEFAULT_ENTRYPOINT = 0x0000000071727De22E5E9d8BAf0edAc6f37da032;
    address constant DEFAULT_WLD = 0x2cFc85d8E48F8EAB294be644d9E25C3030863003;
    address constant DEFAULT_WETH = 0x4200000000000000000000000000000000000006;
    /// @dev SwapRouter02. The legacy `SwapRouter` is not deployed on World Chain.
    address constant DEFAULT_SWAP_ROUTER = 0x091AD9e2e6e5eD44c1c66dB50e49A601F9f36cF6;
    /// @dev Only the 0.3% WLD/WETH tier has meaningful liquidity.
    uint24 constant DEFAULT_POOL_FEE = 3000;
    /// @dev Chainlink-compatible feeds, both 18 decimals.
    address constant DEFAULT_WLD_USD_FEED = 0x8Bb2943AB030E3eE05a58d9832525B4f60A97FA0;
    address constant DEFAULT_ETH_USD_FEED = 0xe1d72a719171DceAB9499757EB9d5AEb9e8D64A6;

    /// @dev ERC-4337 canonical-mempool minimum unstake delay. Bundlers reject less.
    uint32 constant MIN_UNSTAKE_DELAY = 1 days;

    // Defaults sized for World Chain, where the base fee is a fraction of a gwei
    // and a typical op's maxCost lands around 1e-5 ETH. See README "How much ETH
    // does it need?". 0.02 ETH of deposit above a 0.002 ETH floor is on the order
    // of a thousand sponsored ops.
    uint256 constant DEFAULT_DEPOSIT = 0.02 ether;
    uint256 constant DEFAULT_STAKE = 0.05 ether;
    /// @dev Overrides the contract's 0.05 ETH default, which would swallow the
    ///      whole default deposit and leave nothing sponsorable.
    uint256 constant DEFAULT_MIN_ENTRYPOINT_DEPOSIT = 0.002 ether;

    struct Config {
        address entryPoint;
        address wld;
        address weth;
        address router;
        uint24 poolFee;
        uint256 deposit;
        uint256 stake;
        uint32 unstakeDelay;
        uint256 minEntryPointDeposit;
        address newOwner;
    }

    function run() external returns (WLDPaymaster paymaster, IWldEthOracle oracle) {
        Config memory c = _config();

        _preflight(c);

        vm.startBroadcast();

        oracle = _deployOracle(c.wld, c.weth);
        _requireLiveOracle(oracle);

        paymaster = new WLDPaymaster(
            IEntryPoint(c.entryPoint), IERC20(c.wld), IWETH9(c.weth), ISwapRouter(c.router), oracle, c.poolFee
        );

        _configure(paymaster, c);

        paymaster.deposit{value: c.deposit}();
        paymaster.addStake{value: c.stake}(c.unstakeDelay);

        // Ownership transfer last, so all configuration above still succeeds.
        if (c.newOwner != address(0)) {
            paymaster.transferOwnership(c.newOwner);
        }

        vm.stopBroadcast();

        _postflight(c, paymaster, oracle);
        _report(c, paymaster, oracle);
    }

    // =========================================================================
    //                                 config
    // =========================================================================

    /// @notice The resolved deployment config (env with defaults applied).
    /// @dev Public so tests can inspect and mutate it without touching process env.
    function config() public view returns (Config memory) {
        return _config();
    }

    function _config() internal view returns (Config memory c) {
        c.entryPoint = vm.envOr("ENTRYPOINT", DEFAULT_ENTRYPOINT);
        c.wld = vm.envOr("WLD", DEFAULT_WLD);
        c.weth = vm.envOr("WETH", DEFAULT_WETH);
        c.router = vm.envOr("SWAP_ROUTER", DEFAULT_SWAP_ROUTER);
        c.poolFee = uint24(vm.envOr("POOL_FEE", uint256(DEFAULT_POOL_FEE)));
        c.deposit = vm.envOr("DEPOSIT", DEFAULT_DEPOSIT);
        c.stake = vm.envOr("STAKE", DEFAULT_STAKE);
        c.unstakeDelay = uint32(vm.envOr("UNSTAKE_DELAY", uint256(MIN_UNSTAKE_DELAY)));
        c.minEntryPointDeposit = vm.envOr("MIN_ENTRYPOINT_DEPOSIT", DEFAULT_MIN_ENTRYPOINT_DEPOSIT);
        c.newOwner = vm.envOr("OWNER", address(0));
    }

    /// @dev Applies every owner-settable knob that was provided via env.
    function _configure(WLDPaymaster paymaster, Config memory c) internal {
        uint256 v;

        v = vm.envOr("PREMIUM_BPS", uint256(0));
        if (v > 0) paymaster.setPremiumBps(v);

        v = vm.envOr("BLOCKS_PER_BATCH", uint256(0));
        if (v > 0) paymaster.setBlocksPerBatch(v);

        v = vm.envOr("MAX_SWAP_SLIPPAGE_BPS", uint256(0));
        if (v > 0) paymaster.setMaxSwapSlippageBps(v);

        // Sentinel-guarded so an explicit 0 is still applied where 0 is meaningful.
        v = vm.envOr("MAX_WLD_PER_BATCH", type(uint256).max);
        if (v != type(uint256).max) paymaster.setMaxWldPerBatch(v);

        // Always set: the contract's own default is deliberately conservative and
        // would exceed a typical World Chain deposit.
        paymaster.setMinEntryPointDeposit(c.minEntryPointDeposit);

        v = vm.envOr("POSTOP_GAS_OVERHEAD", uint256(0));
        if (v > 0) paymaster.setPostOpGasOverhead(v);

        v = vm.envOr("KEEPER_REWARD_BPS", type(uint256).max);
        if (v != type(uint256).max) paymaster.setKeeperRewardBps(v);
    }

    function _deployOracle(address wld, address weth) internal returns (IWldEthOracle) {
        string memory kind = vm.envOr("ORACLE_KIND", string("chainlink"));
        bytes32 k = keccak256(bytes(kind));

        if (k == keccak256("chainlink")) {
            return new ChainlinkWldEthOracle(
                IAggregatorV3(vm.envOr("WLD_USD_FEED", DEFAULT_WLD_USD_FEED)),
                IAggregatorV3(vm.envOr("ETH_USD_FEED", DEFAULT_ETH_USD_FEED)),
                vm.envOr("MAX_STALENESS", uint256(1 hours))
            );
        }
        if (k == keccak256("twap")) {
            return new UniswapV3TwapOracle(
                vm.envAddress("WLD_WETH_POOL"), wld, weth, uint32(vm.envOr("TWAP_WINDOW", uint256(600)))
            );
        }
        revert("ORACLE_KIND must be 'chainlink' or 'twap'");
    }

    // =========================================================================
    //                          pre / post flight checks
    // =========================================================================

    /// @notice Validates a config without deploying anything.
    /// @dev Public so tests can exercise each guard with an explicit config rather
    ///      than mutating process-global env vars (which races under parallel runs).
    function preflight(Config memory c) public view {
        _preflight(c);
    }

    /// @dev Fail before spending gas if the environment isn't what we think it is.
    function _preflight(Config memory c) internal view {
        _requireCode(c.entryPoint, "ENTRYPOINT");
        _requireCode(c.wld, "WLD");
        _requireCode(c.weth, "WETH");
        _requireCode(c.router, "SWAP_ROUTER");

        require(c.deposit > 0, "DEPOSIT must be > 0 or the paymaster cannot sponsor");
        require(c.stake > 0, "STAKE must be > 0 or bundlers will reject every op");
        require(c.unstakeDelay >= MIN_UNSTAKE_DELAY, "UNSTAKE_DELAY below the 1-day bundler minimum");
        require(tx.origin.balance >= c.deposit + c.stake, "deployer balance < DEPOSIT + STAKE");

        // The floor is reserved, not spendable: DEPOSIT must clear it with room to
        // actually sponsor, or validate() reverts on the very first op.
        require(c.deposit > c.minEntryPointDeposit, "DEPOSIT must exceed MIN_ENTRYPOINT_DEPOSIT (floor is reserved)");
    }

    /// @dev A deployed-but-unpriceable oracle would reject every op; catch it now.
    function _requireLiveOracle(IWldEthOracle oracle) internal view {
        uint256 wldPerEth = oracle.wldForEth(1 ether);
        require(wldPerEth > 0, "oracle returned a zero price");
        require(oracle.ethForWld(wldPerEth) > 0, "oracle inverse returned zero");
    }

    /// @dev Assert the deployed paymaster is actually able to sponsor.
    function _postflight(Config memory c, WLDPaymaster paymaster, IWldEthOracle oracle) internal view {
        require(address(paymaster.entryPoint()) == c.entryPoint, "entryPoint mismatch");
        require(address(paymaster.oracle()) == address(oracle), "oracle mismatch");

        // validate() reverts unless deposit >= maxCost + minEntryPointDeposit, so a
        // deposit at or below the floor cannot sponsor anything at all.
        require(paymaster.getDeposit() == c.deposit, "deposit not credited");
        require(
            paymaster.getDeposit() > paymaster.minEntryPointDeposit(),
            "DEPOSIT <= minEntryPointDeposit: every op would revert"
        );

        IStakeManager.DepositInfo memory info = IStakeManager(c.entryPoint).getDepositInfo(address(paymaster));
        require(info.staked, "paymaster is not staked");
        require(info.stake >= c.stake, "stake not credited");
        require(info.unstakeDelaySec >= MIN_UNSTAKE_DELAY, "unstake delay below bundler minimum");

        // Pricing works end to end through the paymaster, premium included.
        require(paymaster.quoteWldCharge(0.001 ether) > 0, "quote returned zero");
        require(paymaster.maxWldPerBatch() > 0, "maxWldPerBatch=0 leaves batch size unbounded");
    }

    function _requireCode(address a, string memory name) internal view {
        require(a != address(0), string.concat(name, " is the zero address"));
        require(a.code.length > 0, string.concat(name, " has no code on this chain"));
    }

    // =========================================================================
    //                                 report
    // =========================================================================

    function _report(Config memory c, WLDPaymaster paymaster, IWldEthOracle oracle) internal view {
        console2.log("");
        console2.log("=== deployed & ready to sponsor ===");
        console2.log("chain id:             ", block.chainid);
        console2.log("paymaster:            ", address(paymaster));
        console2.log("oracle:               ", address(oracle));
        console2.log("owner:                ", paymaster.owner());
        console2.log("");
        console2.log("EntryPoint deposit:   ", paymaster.getDeposit());
        console2.log("  deposit floor:      ", paymaster.minEntryPointDeposit());
        console2.log("  sponsorable:        ", paymaster.getDeposit() - paymaster.minEntryPointDeposit());
        console2.log("EntryPoint stake:     ", c.stake);
        console2.log("  unstake delay (s):  ", c.unstakeDelay);
        console2.log("");
        console2.log("premium bps:          ", paymaster.premiumBps());
        console2.log("blocks per batch:     ", paymaster.blocksPerBatch());
        console2.log("max swap slippage bps:", paymaster.maxSwapSlippageBps());
        console2.log("max WLD per batch:    ", paymaster.maxWldPerBatch());
        console2.log("postOp gas overhead:  ", paymaster.postOpGasOverhead());
        console2.log("WLD per 1 ETH (+prem):", paymaster.quoteWldCharge(1 ether));
        console2.log("");
        console2.log("--- remaining manual steps ---");
        console2.log("1. Users must approve() the paymaster to spend their WLD.");
        console2.log("2. Clients must set a NON-ZERO paymasterPostOpGasLimit in");
        console2.log("   paymasterAndData, or v0.7 skips postOp: no refund is issued");
        console2.log("   and nothing is booked for batch settlement.");
        console2.log("3. Whitelist the paymaster on the World Chain Rundler sidecar.");
        if (paymaster.owner() == tx.origin) {
            console2.log("4. WARNING: owner is the deploying EOA. It can withdraw the whole");
            console2.log("   deposit and replace the price oracle. Set OWNER=<multisig>.");
        }
    }
}
