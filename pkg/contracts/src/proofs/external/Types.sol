// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title Types
/// @notice Hand-written, ABI-matched reimplementation of the OP Stack dispute
///         user-defined value types and shared enums/structs, pinned to
///         `op-contracts/v3.0.0-rc.2`. Reimplemented (not vendored) so the
///         World Chain proof system can satisfy the canonical `IDisputeGame`
///         surface without depending on the optimism monorepo.

/// @notice A claim represents an MPT root representing the state of the fault proof program.
type Claim is bytes32;

/// @notice A `Hash` type. Feel free to use this whenever you would otherwise use a `bytes32`.
type Hash is bytes32;

/// @notice A dedicated timestamp type.
type Timestamp is uint64;

/// @notice A dedicated duration type.
/// @dev Unit: seconds
type Duration is uint64;

/// @notice The `GameType` type is used to identify the type of dispute game.
type GameType is uint32;

/// @notice A `GameId` represents a packed 1 byte game ID, an 11 byte timestamp, and a 20 byte address.
/// @dev The packed layout of this type is as follows:
/// ┌───────────┬───────────┐
/// │   Bits    │   Value   │
/// ├───────────┼───────────┤
/// │ [0, 32)   │ Game Type │
/// │ [32, 96)  │ Timestamp │
/// │ [96, 256) │ Address   │
/// └───────────┴───────────┘
type GameId is bytes32;

/// @notice The current status of the dispute game.
/// @dev Enum order is canonical and MUST NOT be changed: it matches the OP Stack
///      `GameStatus` ordering relied upon by the `AnchorStateRegistry` and tooling.
enum GameStatus {
    // The game is currently in progress, and has not been resolved.
    IN_PROGRESS,
    // The game has concluded, and the `rootClaim` was challenged successfully.
    CHALLENGER_WINS,
    // The game has concluded, and the `rootClaim` could not be contested.
    DEFENDER_WINS
}

/// @notice Represents the mode in which bonds are distributed after a game resolves.
enum BondDistributionMode {
    // Bond distribution mode has not been decided yet.
    UNDECIDED,
    // Bonds are distributed normally to the prevailing side.
    NORMAL,
    // Bonds are refunded to their original depositors.
    REFUND
}

/// @notice An `OutputRoot` an output root claim and a corresponding L2 block number.
struct OutputRoot {
    Hash root;
    uint256 l2BlockNumber;
}

/// @title LibClaim
/// @notice `.raw()` wrapper for the `Claim` user-defined value type.
library LibClaim {
    function raw(Claim _claim) internal pure returns (bytes32 claim_) {
        claim_ = Claim.unwrap(_claim);
    }
}

/// @title LibHash
/// @notice `.raw()` wrapper for the `Hash` user-defined value type.
library LibHash {
    function raw(Hash _hash) internal pure returns (bytes32 hash_) {
        hash_ = Hash.unwrap(_hash);
    }
}

/// @title LibGameType
/// @notice `.raw()` wrapper for the `GameType` user-defined value type.
library LibGameType {
    function raw(GameType _gameType) internal pure returns (uint32 gameType_) {
        gameType_ = GameType.unwrap(_gameType);
    }
}

/// @title LibTimestamp
/// @notice `.raw()` wrapper for the `Timestamp` user-defined value type.
library LibTimestamp {
    function raw(Timestamp _timestamp) internal pure returns (uint64 timestamp_) {
        timestamp_ = Timestamp.unwrap(_timestamp);
    }
}

/// @title LibDuration
/// @notice `.raw()` wrapper for the `Duration` user-defined value type.
library LibDuration {
    function raw(Duration _duration) internal pure returns (uint64 duration_) {
        duration_ = Duration.unwrap(_duration);
    }
}

using LibClaim for Claim global;
using LibHash for Hash global;
using LibGameType for GameType global;
using LibTimestamp for Timestamp global;
using LibDuration for Duration global;
