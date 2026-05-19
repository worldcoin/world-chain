// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {UpgradeableBeacon} from "@openzeppelin/contracts/proxy/beacon/UpgradeableBeacon.sol";

/// @title WorldChainAccountUpgradeableBeacon
/// @author 0xOsiris, World Contributors
/// @notice The single global beacon resolved by every `WorldChainAccountBeaconProxy`. Its
///         `implementation()` points at the active `WorldChainAccount` contract
/// @custom:security-contact security@toolsforhumanity.com
contract WorldChainAccountUpgradeableBeacon is UpgradeableBeacon {
    constructor(address implementation_, address initialOwner) UpgradeableBeacon(implementation_, initialOwner) {}
}
