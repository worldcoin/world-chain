// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @dev Minimal stand-in for the OP `SystemConfig` covering the members the
///      `AnchorStateRegistry` and `DelayedWETH` actually consult.
contract MockSystemConfig {
    address public guardian;
    bool public paused;
    address public superchainConfig;

    constructor(address guardian_) {
        guardian = guardian_;
    }

    function setGuardian(address guardian_) external {
        guardian = guardian_;
    }

    function setPaused(bool paused_) external {
        paused = paused_;
    }
}
