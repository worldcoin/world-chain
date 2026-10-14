// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IActionVerifier} from "../../src/interfaces/IActionVerifier.sol";

/// @title MockActionVerifier
/// @notice Test double for `IActionVerifier`. Either always-accepts / always-rejects, or
///         accepts only one configured `action`. Used by `WorldChainRpSignerImplV1` tests to
///         exercise the multi-verifier routing path independent of `SubsidyAccounting` state.
contract MockActionVerifier is IActionVerifier {
    bool public acceptAll;
    uint256 public allowedAction;
    bool public hasAllowedAction;

    constructor(bool acceptAll_) {
        acceptAll = acceptAll_;
    }

    function setAcceptAll(bool acceptAll_) external {
        acceptAll = acceptAll_;
    }

    function setAllowedAction(uint256 action) external {
        allowedAction = action;
        hasAllowedAction = true;
    }

    function clearAllowedAction() external {
        allowedAction = 0;
        hasAllowedAction = false;
    }

    function isValidAction(uint256 action) external view returns (bool) {
        if (acceptAll) return true;
        if (hasAllowedAction && action == allowedAction) return true;
        return false;
    }
}
