// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IWorldChainAccountRouterErrors} from "../interfaces/IWorldChainAccountRouterErrors.sol";

/// @title Dispatch
/// @author 0xOsiris, World Contributors
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

    /// @notice Handles unknown selectors that miss the derived contracts `external` entry points.
    /// @param selector The leading 4 bytes of the inbound calldata.
    function _onUnknownSelector(bytes4 selector) internal virtual {
        revert IWorldChainAccountRouterErrors.UnknownSelector(selector);
    }
}
