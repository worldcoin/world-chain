// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @dev Minimal stand-in for the OP `SystemConfig` covering the members the
///      `AnchorStateRegistry` and `DelayedWETH` actually consult.
contract MockSystemConfig {
    address public guardian;
    uint256 public l2ChainId;
    bool public paused;
    address public superchainConfig;

    constructor(address guardian_, uint256 l2ChainId_) {
        guardian = guardian_;
        l2ChainId = l2ChainId_;
    }

    function setGuardian(address guardian_) external {
        guardian = guardian_;
    }

    function setPaused(bool paused_) external {
        paused = paused_;
    }
}
