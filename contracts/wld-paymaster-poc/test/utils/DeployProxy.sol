// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {IEntryPoint} from "@account-abstraction/interfaces/IEntryPoint.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

import {WLDPaymaster} from "../../src/WLDPaymaster.sol";
import {IWETH9, ISwapRouter} from "../../src/interfaces/ISwapRouter.sol";
import {IWldEthOracle} from "../../src/interfaces/IWldEthOracle.sol";

/// @dev Deploys the paymaster the way production does — implementation behind an
///      ERC-1967 proxy — so tests exercise the delegatecall path, not the raw
///      implementation.
library DeployProxy {
    function deploy(
        IEntryPoint entryPoint,
        IERC20 wld,
        IWETH9 weth,
        ISwapRouter router,
        IWldEthOracle oracle,
        uint24 poolFee,
        address owner
    ) internal returns (WLDPaymaster paymaster, address implementation) {
        implementation = address(new WLDPaymaster());
        bytes memory initData =
            abi.encodeCall(WLDPaymaster.initialize, (entryPoint, wld, weth, router, oracle, poolFee, owner));
        paymaster = WLDPaymaster(payable(address(new ERC1967Proxy(implementation, initData))));
    }
}
