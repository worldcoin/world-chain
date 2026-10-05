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
proof-timeout resolution permits the next attempt to be created. For anchor advancement, the
proposer walks resolved defender-winning games newest-to-oldest and closes the first game whose
claim is valid according to the registry, allowing it to skip a newer game still in its airgap.

## Retry operations

The automated services assume proof-timeout retries are exceptional. The proposer creates the next
attempt and the defender follows that replacement. Games descending from the abandoned attempt
become resolvable as `INVALID_PARENT`; the bond manager keeps proposer-owned games tracked, resolves
those descendants as their parents settle, and closes them to release their bonds. Retry creation remains
logged at warn level for operator visibility.

## Recovering from an unusable anchor

Restart both proposer and defender with `--recovery-parent <P>` (or `RECOVERY_PARENT=<P>`),
where P is an eligible ancestor of the unusable anchor. While the ASR anchor is unusable,
both services select the replacement lineage from P using the active implementation's domain.
The proposer creates and resolves replacement games; the defender supplies their proofs.
Existing replacement games are rediscovered across ticks and restarts.

P must be a registered, proper, respected game resolved with `DEFENDER_WINS`, and its root must
match the canonical finalized L2 root. Eligibility and root checks are repeated while P is used.
Invalid parents or failed RPC reads stop that scan without falling back to the unusable anchor.
The ASR sentinel and zero address cannot be recovery parents.

Both services automatically resume from the ASR once it has a claim-valid anchor at a block
above P. A replacement at the old anchor's block cannot advance the ASR; a later replacement
must resolve and pass the registry finality delay. Remove the flag after recovery to avoid
reusing P during a subsequent incident. `lineage.recovery_active` reports whether recovery
selection is active, and startup logs include the configured parent.

The flag does not change onchain state or bypass parent and retry checks. Replacing a blacklisted
successful game with the same root and parent requires a new proof domain to avoid its occupied
factory UUID. Activate that domain through governance before restarting the services. Keep the
block interval unchanged for this recovery procedure; the flag does not reconcile cadence changes.
If P is retired or no eligible game parent exists, onchain recovery is required.

### `root_claim`

- rpc request to a consensus client - i.e. `optimism_outputAtBlock`

### `l2_block_number`

- `parent_ref`'s `l2_block_number` plus the registered proof domain's block interval

## Bond settlement

Bonds are locked from the proposer's available balance in the singleton ERC-20 staking vault.
After a game resolves and passes the registry's finality airgap, its permissionless `closeGame()`
call settles the complete bond pot into immediately reusable vault balances. The service never
requests an external bond-token withdrawal. The bond manager keeps every discovered proposer-owned
game tracked until it is resolved and settled. For games
whose embedded proposal domain differs from the currently registered domain, it also submits any
available positive or negative resolution because those games are no longer visible to the selected
lineage proposer. Same-domain outcomes remain with the proposer to avoid racing retry creation.
