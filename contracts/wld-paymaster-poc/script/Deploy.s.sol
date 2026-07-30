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
 * @notice Example deployment wiring. Fill in the World Chain addresses via env vars.
 *
 * Required env:
 *   ENTRYPOINT       - ERC-4337 EntryPoint v0.7
 *   WLD              - WLD token
 *   WETH             - WETH9
 *   SWAP_ROUTER      - Uniswap V3 SwapRouter
 *   POOL_FEE         - fee tier used for the batch swap (e.g. 3000)
 *   INITIAL_DEPOSIT  - optional: wei to seed the EntryPoint deposit
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

    function run() external {
        address entryPoint = vm.envAddress("ENTRYPOINT");
        address wld = vm.envAddress("WLD");
        address weth = vm.envAddress("WETH");
        address router = vm.envAddress("SWAP_ROUTER");
        uint24 poolFee = uint24(vm.envUint("POOL_FEE"));
        uint256 initialDeposit = vm.envOr("INITIAL_DEPOSIT", uint256(0));
        string memory oracleKind = vm.envOr("ORACLE_KIND", string("chainlink"));

        vm.startBroadcast();

        IWldEthOracle oracle = _deployOracle(oracleKind, wld, weth);

        WLDPaymaster paymaster =
            new WLDPaymaster(IEntryPoint(entryPoint), IERC20(wld), IWETH9(weth), ISwapRouter(router), oracle, poolFee);

        if (initialDeposit > 0) {
            paymaster.deposit{value: initialDeposit}();
        }

        vm.stopBroadcast();

        console2.log("Oracle kind:", oracleKind);
        console2.log("Oracle:    ", address(oracle));
        console2.log("Paymaster: ", address(paymaster));
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
