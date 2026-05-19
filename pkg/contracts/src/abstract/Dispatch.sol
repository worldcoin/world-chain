// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IWorldChainAccountRouterErrors} from "../interfaces/IWorldChainAccountRouterErrors.sol";

/// @title Dispatch
/// @author 0xOsiris, World Contributors
/// @notice Diamond-style fallback entry point. The router's system selectors are bound at compile
///         time via Solidity's native function dispatcher on the derived router; only calls whose
///         selector matches none of those system selectors arrive here. The dispatch entry decodes
///         the leading 4 bytes of `msg.data` and forwards control to the virtual
///         `_onUnknownSelector(bytes4)` extension point.
/// @custom:security-contact security@toolsforhumanity.com
abstract contract Dispatch {
    /// @notice Accepts plain ETH transfers. Calls with empty calldata short-circuit dispatch so
    ///         the account can be used as a value sink.
    receive() external payable virtual {}

    /// @notice Catches every selector that the derived router does not declare as an `external`
    ///         function. Routes the call to `_onUnknownSelector` for extension handling.
    /// @dev Calldata shorter than 4 bytes is also routed; the resulting selector is right-padded
    ///      from `calldataload(0)`, which is the same convention Solidity uses for unknown-call
    ///      detection.
    fallback() external payable virtual {
        bytes4 selector;
        assembly ("memory-safe") {
            selector := shr(224, calldataload(0))
        }
        _onUnknownSelector(selector);
    }

    /// @notice Extension hook invoked for every selector that misses Solidity's native dispatch.
    ///         Default behavior reverts. Override to plug a per-account fallback handler. An
    ///         override that returns control to the caller will yield empty returndata; an
    ///         override that wants to forward the callee's returndata MUST terminate the call
    ///         frame itself (via inline assembly `return`/`revert`).
    /// @param selector The leading 4 bytes of the inbound calldata.
    function _onUnknownSelector(bytes4 selector) internal virtual {
        revert IWorldChainAccountRouterErrors.UnknownSelector(selector);
    }
}
