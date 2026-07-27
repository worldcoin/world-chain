// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {GameType} from "@optimism-bedrock/src/dispute/lib/Types.sol";

/// @notice OP Stack dispute-game type allocations owned by World Chain.
library WorldChainGameTypes {
    GameType internal constant WIP_1006 = GameType.wrap(1006);
}
