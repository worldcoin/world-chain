---
name: invalid-parent
description: Trace a WIP1006 INVALID_PARENT cascade to its first ancestor with a different status or invalidation reason.
---

Run `python3 scripts/trace_parent.py <game-address> [mainnet|sepolia]`, resolving
the script relative to this skill directory. Execute it directly; reading or
recreating the script is unnecessary unless troubleshooting.

Default to mainnet for Ethereum/mainnet or no network; use sepolia when specified.
The script reads `ETHEREUM_PROVIDER` or `ETHEREUM_SEPOLIA_PROVIDER` respectively.
It requires only Python's standard library and a provider supporting JSON-RPC batches.

The script verifies the initial game reports CHALLENGER_WINS / INVALID_PARENT,
then follows parentRef() to the first ancestor where either value differs.
Reads use one block, with bounded requests, cycle detection, and a 200-hop limit.
Failures exit nonzero; partial rows are not a completed trace.

Report the terminal address, GameStatus, InvalidationReason, and parent-hop count.
Include the chain when short. This identifies the on-chain origin of the cascade,
not the underlying cause of a proof timeout, and does not authenticate contract type.
