# World Chain Proposer

This crate contains the world chain proposer.

## Goal

Periodically post L2 output root to L1.

## How

Propose a new L2 output root by creating a WIP-1006 `MultiProofGame` clone through the stock OP
Stack `DisputeGameFactory.create(gameType, rootClaim, extraData)`.

## Items needed to propose a new L2 output root

- `parent_ref`: address of the parent game, or the `AnchorStateRegistry` contract address when the
  proposal extends the current anchor.
- `root_claim`: OP stack output root.
- `l2_block_number`: L2 block number for the root claim.
- `attempt`: retry nonce, non-zero only when replacing a game invalidated by a proof timeout.
- `retry_of`: concrete previous game for a non-zero attempt.
- `l1_origin_hash` and `l1_origin_number`: recent L1 block selected by the proposer.
- `creation_proof`: Nitro proof verified by the game during creation.

These fields determine the factory call:
`extraData = abi.encode(domainHash, l2BlockNumber, parentRef, attempt, retryOf, l1OriginHash,
l1OriginNumber, creationProof)`.

## How to get these items

### `parent_ref`

- start with `parent_ref` equal to the `AnchorStateRegistry` address. Once the anchor advances onto
  a game, that game is no longer a valid parent — `MultiProofGame.initialize` rejects a parent at or
  below the anchor — so new proposals extending the anchor always point at the registry.
- compute L2 output root for block equal to `parent_ref`'s `l2_block_number` + `BLOCK_INTERVAL`
- take one paginated `DisputeGameFactory.findLatestGames` snapshot back to the current anchor game,
  cache it for the observed factory count, then group games by transition and follow their explicit
  retry lineage in memory. At the anchor tip both the registry and the current anchor game are
  candidate parents, because a game created before the anchor advanced still references the anchor
  game. Games from re-registered implementations with another domain or `extraData` layout are
  ignored.
- if a game exists, it becomes the `parent_ref` and we continue this loop
- if it doesn't exist - i.e. the address is `0x00..00`, then the current `parent_ref` is returned

### `root_claim`

- rpc request to a consensus client - i.e. `optimism_outputAtBlock`

### `l2_block_number`

- `parent_ref`'s `l2_block_number` field + `BLOCK_INTERVAL`

### Creation proof and L1 origin

- request a Nitro proof against a finalized L1 head;
- encode its transition values, signature, and registered enclave public key in `creation_proof`;
- reject and re-request a proof once its L1 origin is more than 8,000 blocks old, leaving
  transaction-inclusion headroom inside EIP-2935's 8,191-block history window.

## Devnet coverage

The ignored full-stack E2E covers proof-backed game creation, the stock Portal prove/finalize
withdrawal flow, anchor advancement, and both DelayedWETH bond-claim phases. The devnet uses a
`MockRootIdVerifier` for the creation lane, so production Nitro key registration and verification
remain separate integration coverage.

## Bond settlement

Bonds are custodied in `DelayedWETH` and paid out in two phases. The first `claimCredit(recipient)`
call unlocks the credit; the second, after the WETH delay, withdraws and transfers it. Both are
gated on `AnchorStateRegistry.isGameFinalized`, since `claimCredit` calls `closeGame`, which reverts
until the registry's finality airgap has elapsed. The bond manager keeps a game tracked until its
pending withdrawal is drained.
