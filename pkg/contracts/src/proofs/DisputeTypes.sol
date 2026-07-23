// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

// ABI-compatible with the OP Stack dispute-game types consumed by the Portal-facing
// World Chain interfaces. Keep these widths and enum ordering aligned with the pinned OP contracts.
type Claim is bytes32;
type GameId is bytes32;
type Hash is bytes32;
type Timestamp is uint64;
type GameType is uint32;

enum GameStatus {
    IN_PROGRESS,
    CHALLENGER_WINS,
    DEFENDER_WINS
}

struct Proposal {
    Hash root;
    uint256 l2SequenceNumber;
}

library GameTypes {
    GameType internal constant WIP_1006 = GameType.wrap(1006);
}
