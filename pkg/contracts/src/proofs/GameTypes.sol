// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {GameType} from "@optimism-bedrock/src/dispute/lib/Types.sol";

/// @title GameTypes
/// @author World Contributors
/// @custom:security-contact security@toolsforhumanity.com
library GameTypes {
    GameType internal constant MULTI_PROOF_GAME_TYPE = GameType.wrap(1006);
}
