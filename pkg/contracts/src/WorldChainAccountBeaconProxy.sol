// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {BeaconProxy} from "@openzeppelin/contracts/proxy/beacon/BeaconProxy.sol";

/// @title WorldChainAccountBeaconProxy
/// @author 0xOsiris, World Contributors
/// @custom:security-contact security@toolsforhumanity.com
contract WorldChainAccountBeaconProxy is BeaconProxy {
    /// @notice The single global `WorldChainAccountUpgradeableBeacon` predeploy. See WIP-1001
    ///         "Beacon-Proxied Account Router" for the role of this address in the WIP-1001
    ///         account architecture.
    address internal constant WORLD_CHAIN_ACCOUNT_BEACON = 0x000000000000000000000000000000000000B0cC;

    constructor() BeaconProxy(WORLD_CHAIN_ACCOUNT_BEACON, "") {}
}
