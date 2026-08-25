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
- read the block interval from the registered game's proof domain and compute the L2 output root
  for `parent_ref`'s `l2_block_number` plus that interval
- look the game up with `DisputeGameFactory.games(gameType, rootClaim, extraData)`, walking
  `attempt` upward until the first gap.
- if a game exists, it becomes the `parent_ref` and we continue this loop
- if it doesn't exist - i.e. the address is `0x00..00`, then the current `parent_ref` is returned

The proposer resolves every determined game parent-first on this selected lineage. A child may
resolve as soon as its parent resolves successfully, so consecutive games' registry finality
windows can overlap. A positive resolution may advance the anchor after its own finality delay; a
proof-timeout resolution permits the next attempt to be created.

## Retry operations

The automated services assume proof-timeout retries are exceptional. The proposer creates the next
attempt and the defender follows that replacement. Games descending from the abandoned attempt
become resolvable as `INVALID_PARENT`; the bond manager keeps proposer-owned games tracked, resolves
those descendants as their parents settle, and claims the refunded bonds. Retry creation remains
logged at warn level for operator visibility.

### `root_claim`

- rpc request to a consensus client - i.e. `optimism_outputAtBlock`

### `l2_block_number`

- `parent_ref`'s `l2_block_number` plus the registered proof domain's block interval

## Bond settlement

Bonds are custodied in `DelayedWETH` and paid out in two phases. The first `claimCredit(recipient)`
call unlocks the credit; the second, after the WETH delay, withdraws and transfers it. Both are
gated on `AnchorStateRegistry.isGameFinalized`, since `claimCredit` calls `closeGame`, which reverts
until the registry's finality airgap has elapsed. The bond manager keeps every discovered
proposer-owned game tracked until it is resolved and its pending withdrawal is drained. For games
whose embedded proposal domain differs from the currently registered domain, it also submits any
available positive or negative resolution because those games are no longer visible to the selected
lineage proposer. Same-domain outcomes remain with the proposer to avoid racing retry creation.
