---
name: pending-games
description: List unresolved WIP1006 dispute game addresses by scanning backward through at most the newest 2000 entries in a supplied factory.
---

Run `python3 scripts/pending_games.py <factory-address> [mainnet|sepolia]`,
resolving the script relative to this skill directory. Execute it directly;
reading or recreating it is unnecessary unless troubleshooting.

The user supplies the dispute game factory. Default to mainnet; use sepolia
when specified. L1 comes from `ETHEREUM_PROVIDER` or
`ETHEREUM_SEPOLIA_PROVIDER`, respectively. Requires Python's standard library,
the adjacent investigate-game RPC helper, and a provider supporting JSON-RPC
batches and EIP-1898 canonical block-hash reads. No transactions are submitted.

Pending means game type `1006` with stored `status() == IN_PROGRESS` (`0`).
Include expired, challenged, proven, blacklisted, or retired games when their
stored status remains in progress. Deadline expiry and eligibility to resolve
do not mean the game has resolved. Resolved games awaiting `closeGame()` or
registry finality are excluded.

The script checks the L1 chain ID and scans backward from the newest factory
entry at one latest L1 snapshot, in batches of 50, up to 2000 factory entries
(including non-1006 types). It does not stop at a resolved game or an expired
deadline: older games may still be pending, including across implementation
changes. Reads stay pinned to the canonical snapshot hash; a reorg fails the
scan rather than mixing snapshots. The supplied factory is the trust root,
not independently authenticated as a canonical World Chain deployment.

On success, return `pending_games` as full, copyable addresses, one per line in
a code block, newest first. Briefly include network, factory, pending count,
entries scanned, and L1 snapshot number/hash. If `older_entries_unscanned` is
nonzero, state that older games were not checked and the list covers only the
newest 2000 factory entries. An empty list means no pending WIP1006 games in
the scanned range at that snapshot. Exit 1 means a failed scan or RPC/decoding
failure; report the failure, never a partial list as complete. Never expose
RPC URLs or credentials.
