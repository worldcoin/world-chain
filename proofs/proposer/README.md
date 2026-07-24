# World Chain Proposer

This crate contains the world chain proposer.

## Goal

Periodically post L2 output root to L1.

## How

Create a `WorldChainProofSystemGame` through the stock OP Stack
`DisputeGameFactory.create(gameType, rootClaim, extraData)` entrypoint. WIP-1006
uses game type `1006` and:

```text
extraData = abi.encode(domainHash, l2BlockNumber, parentRef, attempt)
```

## Proposal Inputs

- `parentRef`: the parent WIP-1006 game, or the stock `AnchorStateRegistry` when
  extending its current anchor.
- `rootClaim`: the OP Stack output root returned by `optimism_outputAtBlock`.
- `l2BlockNumber`: the parent's L2 block number plus the game implementation's
  configured block interval.
- `domainHash`: read from the registered WIP-1006 implementation.
- `attempt`: zero for a new transition. The proposer increments it only when the
  previous attempt timed out on proofs or was created before WIP-1006 became
  respected.

## Lineage Discovery

The proposer starts at `AnchorStateRegistry.getAnchorRoot()`, scans the shared
factory for the WIP-1006 transition at each next block, and follows valid games
until it finds the canonical tip. It filters every global factory read by game
type `1006`; other OP Stack game types are unrelated. The local
`transitionKey` is used only for logs and in-memory indexing. Factory identity
is the stock OP `gameUUID`, which also commits to `attempt`.
