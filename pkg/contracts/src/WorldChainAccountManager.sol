// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";

/// @title WorldChainAccountManager
/// @author Worldcoin
/// @notice ERC-1967 proxy for the WIP-1001 World Chain Account Manager predeploy.
/// @dev All program state lives on the implementation (`WorldChainAccountManagerImplV1` and
///      successors) inside an EIP-7201 namespaced storage struct. This proxy contract MUST
///      hold no state of its own.
/// @custom:security-contact security@toolsforhumanity.com
contract WorldChainAccountManager is ERC1967Proxy {
    ///////////////////////////////////////////////////////////////////////////////
    ///                    !!!! DO NOT ADD MEMBERS HERE !!!!                    ///
    ///////////////////////////////////////////////////////////////////////////////

    ///////////////////////////////////////////////////////////////////////////////
    ///                              CONSTRUCTION                               ///
    ///////////////////////////////////////////////////////////////////////////////

    /// @param implementation_ Initial implementation address.
    /// @param data If non-empty, `delegatecall`ed against `implementation_` during
    ///        construction. Typically the ABI-encoded `initialize` call.
    constructor(address implementation_, bytes memory data) payable ERC1967Proxy(implementation_, data) {
        // !!!! DO NOT PUT PROGRAM LOGIC HERE !!!!
        // It must live in the `initialize` function of the implementation instead.
    }
}
