# World Chain Proposer

This crate contains the world chain proposer.

## Goal

Periodically post L2 output root to L1.

## How

Propose a new L2 output root by creating a WIP-1006 `MultiProofGame` clone through the stock OP
Stack `DisputeGameFactory.create(gameType, rootClaim, extraData)`.

## Items needed to propose a new L2 output root

- `parent_ref`: address of the current anchor game or a descendant game. The
  `AnchorStateRegistry` address is used only before the first game is anchored.
- `root_claim`: OP stack output root.
- `l2_block_number`: L2 block number for the root claim.
- `attempt`: retry nonce, non-zero only when replacing a game invalidated by a proof timeout.

These four fields determine the factory call: `extraData = abi.encode(domainHash, l2BlockNumber,
parentRef, attempt)` and the game's factory UUID is
`keccak256(abi.encode(gameType, rootClaim, extraData))`.

## How to get these items

### `parent_ref`

- read the current anchor game from `AnchorStateRegistry`. Use it as `parent_ref` when present;
  otherwise use the registry address as the initial sentinel.
- compute L2 output root for block equal to `parent_ref`'s `l2_block_number` + `BLOCK_INTERVAL`
- look the game up with `DisputeGameFactory.games(gameType, rootClaim, extraData)`, walking
  `attempt` upward until the first gap.
- if a game exists, it becomes the `parent_ref` and we continue this loop
- if it doesn't exist - i.e. the address is `0x00..00`, then the current `parent_ref` is returned

### `root_claim`

- rpc request to a consensus client - i.e. `optimism_outputAtBlock`

### `l2_block_number`

- `parent_ref`'s `l2_block_number` field + `BLOCK_INTERVAL`

## Bond settlement

Bonds are custodied in `DelayedWETH` and paid out in two phases. The first `claimCredit(recipient)`
call unlocks the credit; the second, after the WETH delay, withdraws and transfers it. Both are
gated on `AnchorStateRegistry.isGameFinalized`, since `claimCredit` calls `closeGame`, which reverts
until the registry's finality airgap has elapsed. The bond manager keeps a game tracked until its
pending withdrawal is drained.
