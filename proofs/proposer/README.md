# World Chain Proposer

This crate contains the world chain proposer.

## Goal

Periodically post L2 output root to L1.

## How

Propose a new L2 output root by creating a new `WorldChainProofSystemGame` contract through the `WorldChainProofSystemFactory.propose(..)` contract fn.

## Items needed to propose a new L2 output root

- `parent_ref`: address of the parent game, or the `WorldChainAnchorStateRegistry` contract address if there is no parent game.
- `root_claim`: OP stack output root.
- `l2_block_number`: L2 block number for the root claim.

## How to get these items

### `Parent_ref`

- start with `parent_ref` equal to `WorldChainAnchorStateRegistry`
- compute L2 output root for block equal to `parent_ref`'s `l2_block_number` + `BLOCK_INTERVAL` (a protocol constant that we don't currently have, therefore we need to add it)
- compute the proposal key from `domain_hash`, `parent_ref`, `root_claim`, and `l2_block_number`
- check whether the game already exists
- if so, this game becomes the `parent_ref` and we continue this loop
- if it doesn't exist - i.e. the address is `0x00..00`, then the current `parent_ref` is returned

### `root_claim`

- rpc request to a consensus client - i.e. `optimism_outputAtBlock`

### `l2_block_number`

- `parent_ref`'s `l2_block_number` field + `BLOCK_INTERVAL`
