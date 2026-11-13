---
name: recovery-parent
description: Find the nearest defender-winning, ASR claim-valid game backward in a WIP-1006 lineage for the proposer and defender recovery-parent flag.
---

Run `python3 scripts/find_parent.py <game-address> <mainnet|sepolia>`, resolving
the script relative to this skill directory. Execute directly; read the script
only when troubleshooting. Interpret "seoplia" as sepolia. Requires Python's
standard library and the adjacent investigate-game RPC helper. No transactions
are submitted.

Use `ETHEREUM_PROVIDER` for mainnet and `ETHEREUM_SEPOLIA_PROVIDER` for sepolia.
The script checks chain ID, initial game type 1006, and registration in the
discovered ASR's factory. Supply `--expected-factory <address>` when a trusted
deployment factory is available from user context. Otherwise retain the returned
provenance warning; self-reported registry/factory wiring is not canonical
deployment authentication. Never expose provider URLs or credentials.

The starting game is included: return it if it qualifies, otherwise follow
`parentRef()` until the first game with `status() == DEFENDER_WINS` and
`ASR.isGameClaimValid(game) == true`. This includes registration, blacklist,
retirement, pause, respected-type and finality-delay checks. A qualifying legacy
game may also be returned. Stop at the ASR sentinel; it is not a recovery parent.
Cycles, unavailable getters, malformed RPC data, and exceeding 200 parent hops
fail explicitly. All reads use one latest canonical block hash so a recent
blacklist is not intentionally hidden behind a finalized snapshot. One reorg
restart is allowed; RPC requests time out after 20 seconds.

On success, report the full `recovery_parent` address first, then
`--recovery-parent <address>`, network, L2 block number, parent-hop count, snapshot,
and any warning. Explain briefly that the script checks onchain claim validity;
the services additionally verify the canonical L2 root. Replacing an occupied
factory UUID still requires a new proof domain or an onchain retry mechanism.

Exit 1 means an incomplete/failed lookup, not proof that no valid ancestor exists.
Exit 2 means the traversal reached the ASR sentinel without a qualifying game.
In either case, do not recommend a parent address from partial results.
