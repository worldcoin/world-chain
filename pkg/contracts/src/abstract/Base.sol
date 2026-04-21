// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Ownable2StepUpgradeable} from "@openzeppelin/contracts-upgradeable/access/Ownable2StepUpgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";

/// @title Base Delegated Implementation Contract
/// @author Worldcoin
abstract contract Base is Ownable2StepUpgradeable, UUPSUpgradeable {
    /// @notice Initializes the contract with the given owner.
    ///
    /// @param owner The address that will be set as the owner of the contract.
    function __Base_init(address owner) internal virtual onlyInitializing {
        __Ownable_init(owner);
        __UUPSUpgradeable_init();
    }

    /// @notice Is called when upgrading the contract to check whether it should be performed.
    ///
    /// @param newImplementation The address of the implementation being upgraded to.
    ///
    /// @custom:reverts string If called by any account other than the proxy owner.
    function _authorizeUpgrade(address newImplementation) internal virtual override onlyProxy onlyOwner {}

    /**
     * @dev This empty reserved space is put in place to allow future versions to add new
     * variables without shifting down storage in the inheritance chain.
     * See https://docs.openzeppelin.com/contracts/4.x/upgradeable#storage_gaps
     */
    uint256[49] private __gap;
}
