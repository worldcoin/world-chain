// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {Script, console2} from "forge-std/Script.sol";
import {WLDPaymaster} from "../src/WLDPaymaster.sol";
import {ChainlinkWldEthOracle} from "../src/oracle/ChainlinkWldEthOracle.sol";
import {UniswapV3TwapOracle} from "../src/oracle/UniswapV3TwapOracle.sol";
import {IEntryPoint} from "@account-abstraction/interfaces/IEntryPoint.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IWETH9, ISwapRouter} from "../src/interfaces/ISwapRouter.sol";
import {IWldEthOracle} from "../src/interfaces/IWldEthOracle.sol";
import {IAggregatorV3} from "../src/interfaces/IAggregatorV3.sol";

/**
 * @notice Deployment wiring for the WLD paymaster.
 *
 * All addresses default to live World Chain mainnet (chain id 480); override via
 * env for other networks.
 *
 * Env (all optional — defaults are World Chain mainnet):
 *   ENTRYPOINT       - ERC-4337 EntryPoint v0.7
 *   WLD              - WLD token
 *   WETH             - WETH9
 *   SWAP_ROUTER      - Uniswap SwapRouter02 (NOT the legacy SwapRouter)
 *   POOL_FEE         - fee tier for the batch swap (default 3000, the liquid tier)
 *   INITIAL_DEPOSIT  - wei to seed the EntryPoint deposit (default 0)
 *   STAKE            - wei to stake on the EntryPoint (default 0, see below)
 *   UNSTAKE_DELAY    - stake unlock delay in seconds (default 1 day)
 *   MAX_WLD_PER_BATCH- override the default per-batch swap cap
 *
 * STAKING IS REQUIRED for a live deployment: `validatePaymasterUserOp` writes the
 * paymaster's own associated storage in the WLD contract, which ERC-7562 permits
 * only for a staked entity. An unstaked paymaster will have its ops rejected by
 * standards-compliant bundlers regardless of any sidecar whitelisting.
 *
 * Pricing (default: chainlink):
 *   ORACLE_KIND=chainlink (default)
 *     WLD_USD_FEED     - defaults to the live World Chain WLD/USD feed
 *     ETH_USD_FEED     - defaults to the live World Chain ETH/USD feed
 *     MAX_STALENESS    - max feed answer age in seconds (default 3600)
 *   ORACLE_KIND=twap (fallback)
 *     WLD_WETH_POOL    - Uniswap V3 WLD/WETH pool
 *     TWAP_WINDOW      - TWAP window seconds (e.g. 600)
 */
contract Deploy is Script {
    /// @dev Live World Chain Chainlink-compatible feeds (both 18 decimals).
    address constant DEFAULT_WLD_USD_FEED = 0x8Bb2943AB030E3eE05a58d9832525B4f60A97FA0;
    address constant DEFAULT_ETH_USD_FEED = 0xe1d72a719171DceAB9499757EB9d5AEb9e8D64A6;

    // --- live World Chain mainnet (chain id 480) ---
    address constant DEFAULT_ENTRYPOINT = 0x0000000071727De22E5E9d8BAf0edAc6f37da032;
    address constant DEFAULT_WLD = 0x2cFc85d8E48F8EAB294be644d9E25C3030863003;
    address constant DEFAULT_WETH = 0x4200000000000000000000000000000000000006;
    /// @dev SwapRouter02. The legacy `SwapRouter` is not deployed on World Chain.
    address constant DEFAULT_SWAP_ROUTER = 0x091AD9e2e6e5eD44c1c66dB50e49A601F9f36cF6;
    /// @dev Only the 0.3% WLD/WETH tier has meaningful liquidity.
    uint24 constant DEFAULT_POOL_FEE = 3000;

    function run() external {
        address entryPoint = vm.envOr("ENTRYPOINT", DEFAULT_ENTRYPOINT);
        address wld = vm.envOr("WLD", DEFAULT_WLD);
        address weth = vm.envOr("WETH", DEFAULT_WETH);
        address router = vm.envOr("SWAP_ROUTER", DEFAULT_SWAP_ROUTER);
        uint24 poolFee = uint24(vm.envOr("POOL_FEE", uint256(DEFAULT_POOL_FEE)));
        uint256 initialDeposit = vm.envOr("INITIAL_DEPOSIT", uint256(0));
        uint256 stake = vm.envOr("STAKE", uint256(0));
        uint32 unstakeDelay = uint32(vm.envOr("UNSTAKE_DELAY", uint256(1 days)));
        uint256 maxWldPerBatch = vm.envOr("MAX_WLD_PER_BATCH", uint256(0));
        string memory oracleKind = vm.envOr("ORACLE_KIND", string("chainlink"));

        vm.startBroadcast();

        IWldEthOracle oracle = _deployOracle(oracleKind, wld, weth);

        WLDPaymaster paymaster =
            new WLDPaymaster(IEntryPoint(entryPoint), IERC20(wld), IWETH9(weth), ISwapRouter(router), oracle, poolFee);

        if (maxWldPerBatch > 0) {
            paymaster.setMaxWldPerBatch(maxWldPerBatch);
        }

        if (initialDeposit > 0) {
            paymaster.deposit{value: initialDeposit}();
        }

        // Required for standards-compliant bundlers to accept ops (see header).
        if (stake > 0) {
            paymaster.addStake{value: stake}(unstakeDelay);
        }

        vm.stopBroadcast();

        console2.log("Oracle kind:", oracleKind);
        console2.log("Oracle:    ", address(oracle));
        console2.log("Paymaster: ", address(paymaster));
        console2.log("Deposit:   ", paymaster.getDeposit());
        console2.log("Max WLD/batch:", paymaster.maxWldPerBatch());
        if (stake == 0) {
            console2.log("WARNING: paymaster is NOT staked - bundlers will reject its ops.");
        }
    }

    function _deployOracle(string memory kind, address wld, address weth) internal returns (IWldEthOracle) {
        bytes32 k = keccak256(bytes(kind));

        if (k == keccak256("chainlink")) {
            address wldUsd = vm.envOr("WLD_USD_FEED", DEFAULT_WLD_USD_FEED);
            address ethUsd = vm.envOr("ETH_USD_FEED", DEFAULT_ETH_USD_FEED);
            uint256 maxStaleness = vm.envOr("MAX_STALENESS", uint256(3600));
            return new ChainlinkWldEthOracle(IAggregatorV3(wldUsd), IAggregatorV3(ethUsd), maxStaleness);
        }

        if (k == keccak256("twap")) {
            address pool = vm.envAddress("WLD_WETH_POOL");
            uint32 twapWindow = uint32(vm.envUint("TWAP_WINDOW"));
            return new UniswapV3TwapOracle(pool, wld, weth, twapWindow);
        }

        revert("ORACLE_KIND must be 'chainlink' or 'twap'");
    }
}
