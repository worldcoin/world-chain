// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {GameType} from "@optimism-bedrock/src/dispute/lib/Types.sol";

/// @notice OP Stack dispute-game type allocations owned by World Chain.
library GameTypes {
    GameType internal constant MULTI_PROOF_GAME_TYPE = GameType.wrap(1006);
}
