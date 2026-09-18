---
name: invalid-parent
description: Find the root cause of the cascade of invalid parents WIP1006 games when asked to debug a WIP1006 game that failed with invalid parent as invalidation reason
---

1. You're given an Ethereum address. First make sure this address is a WIP1006 game that is failing with `INVALID_PARENT` as invalidation reason.

To do that:

- do a RPC call to <GAME_ADDRESS> `status()` that returns `GameStatus`, where `GameStatus` is:

```solidity
/// @notice The current status of the dispute game.
enum GameStatus {
    // The game is currently in progress, and has not been resolved.
    IN_PROGRESS,
    // The game has concluded, and the `rootClaim` was challenged successfully.
    CHALLENGER_WINS,
    // The game has concluded, and the `rootClaim` could not be contested.
    DEFENDER_WINS
}
```

- make sure the answer is `CHALLENGER_WINS`. This means that the Ethereum address is a WIP1006 game and it has resolved negatively (i.e. failed).

- do a RPC call to <GAME_ADDRESS> `claimData()` that returns `ClaimData`, where `ClaimData` is:

```solidity
/// @notice The `ClaimData` struct represents the data associated with the root claim.
struct ClaimData {
    ProposalStatus status; // 1 byte                            |
    address challenger; // 20 bytes                             |
    Timestamp deadline; // 8 bytes                              |-- one slot (31 bytes)
    Bitmap proofBitmap; // 1 byte                      |
    InvalidationReason invalidationReason; // 1 byte   |
}

/// @notice The lifecycle of the proposer's claim.
enum ProposalStatus {
    // The initial state of a new proposal.
    Unchallenged,
    // A proposal that has been challenged but not yet proven.
    Challenged,
    // An unchallenged proposal supported by at least one accepted proof lane.
    UnchallengedAndValidProofProvided,
    // A challenged proposal supported by `PROOF_THRESHOLD` distinct proof lanes.
    ChallengedAndValidProofProvided,
    // The final state after resolution, either GameStatus.CHALLENGER_WINS or GameStatus.DEFENDER_WINS.
    Resolved
}

enum InvalidationReason {
    NONE,
    PROOF_TIMEOUT,
    INVALID_PARENT
}

/// The set of proof lanes accepted for a proposal, one bit per `ProofLane`.
type Bitmap is uint8;
```

- make sure the invalidation reason is `INVALID_PARENT`.

2. Look at the parent game by doing a RPC call to <GAME_ADDRESS> `parentRef()` that returns `address`, the address of the parent WIP1006 game.

3. Inspect the parent game:

- if the parent game is also failing with `INVALID_PARENT` as invalidation reason (to ensure that, you need to to the same steps described above for the child game), then go back to point 2 and look for its parent game.
- otherwise, return this parent game contract address, the `GameStatus` and the `InvalidationReason`.

### RPC Endpoint

If the prompt mentions Ethereum mainnet (or simply Ethereum or mainnet), use the $ETHEREUM_PROVIDER as the RPC url, if it mentions Ethereum sepolia (or simply sepolia), then use $ETHEREUM_SEPOLIA_PROVIDER. If it doesn't mention any blockchain, default to Ethereum mainnet.
