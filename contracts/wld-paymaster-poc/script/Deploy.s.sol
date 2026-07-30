// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {Script, console2} from "forge-std/Script.sol";
import {WLDPaymaster} from "../src/WLDPaymaster.sol";
import {UniswapV3TwapOracle} from "../src/oracle/UniswapV3TwapOracle.sol";
import {IEntryPoint} from "@account-abstraction/interfaces/IEntryPoint.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IWETH9, ISwapRouter} from "../src/interfaces/ISwapRouter.sol";
import {IWldEthOracle} from "../src/interfaces/IWldEthOracle.sol";

/**
 * @notice Example deployment wiring. Fill in the World Chain addresses via env vars.
 *
 * Required env:
 *   ENTRYPOINT       - ERC-4337 EntryPoint v0.7
 *   WLD              - WLD token
 *   WETH             - WETH9
 *   SWAP_ROUTER      - Uniswap V3 SwapRouter
 *   WLD_WETH_POOL    - Uniswap V3 WLD/WETH pool (for the TWAP)
 *   POOL_FEE         - fee tier used for the swap (e.g. 3000)
 *   TWAP_WINDOW      - TWAP window seconds (e.g. 600)
 *   INITIAL_DEPOSIT  - wei to seed the EntryPoint deposit
 */
contract Deploy is Script {
    function run() external {
        address entryPoint = vm.envAddress("ENTRYPOINT");
        address wld = vm.envAddress("WLD");
        address weth = vm.envAddress("WETH");
        address router = vm.envAddress("SWAP_ROUTER");
        address pool = vm.envAddress("WLD_WETH_POOL");
        uint24 poolFee = uint24(vm.envUint("POOL_FEE"));
        uint32 twapWindow = uint32(vm.envUint("TWAP_WINDOW"));
        uint256 initialDeposit = vm.envOr("INITIAL_DEPOSIT", uint256(0));

        vm.startBroadcast();

        UniswapV3TwapOracle oracle = new UniswapV3TwapOracle(pool, wld, weth, twapWindow);

        WLDPaymaster paymaster = new WLDPaymaster(
            IEntryPoint(entryPoint),
            IERC20(wld),
            IWETH9(weth),
            ISwapRouter(router),
            IWldEthOracle(address(oracle)),
            poolFee
        );

        if (initialDeposit > 0) {
            paymaster.deposit{value: initialDeposit}();
        }

        vm.stopBroadcast();

        console2.log("Oracle:    ", address(oracle));
        console2.log("Paymaster: ", address(paymaster));
    }
}
