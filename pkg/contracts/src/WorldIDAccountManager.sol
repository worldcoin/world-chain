// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";

/// @title World ID Account Manager
/// @author Worldcoin
/// @notice ERC1967 proxy for the WIP-1001 World ID Account Manager.
/// @custom:security-contact security@toolsforhumanity.com
contract WorldIDAccountManager is ERC1967Proxy {
    ///////////////////////////////////////////////////////////////////////////////
    ///                    !!!! DO NOT ADD MEMBERS HERE !!!!                    ///
    ///////////////////////////////////////////////////////////////////////////////

    ///////////////////////////////////////////////////////////////////////////////
    ///                             CONSTRUCTION                                ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @notice Constructs a new instance of the World ID Account Manager proxy.
    /// @param _logic The initial implementation (delegate) of the contract that this acts as a proxy for.
    /// @param _data If non-empty, used as the data for a `delegatecall` to `_logic`.
    constructor(address _logic, bytes memory _data) payable ERC1967Proxy(_logic, _data) {
        // !!!! DO NOT PUT PROGRAM LOGIC HERE !!!!
        // It should go in the `initialize` function of the delegate instead.
    }
}
