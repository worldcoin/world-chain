---
name: investigate-game
description: Verify an Ethereum address is a WIP1006 dispute game and report its status, claim, bonds, proof configuration, and proposal context in a compact debugging table.
---

Run `python3 scripts/investigate_game.py <game-address> [mainnet|sepolia]`,
resolving the script relative to this skill directory. Execute it directly;
reading or recreating it is unnecessary unless troubleshooting.

Default to mainnet; use sepolia when specified. The script reads
`ETHEREUM_PROVIDER` or `ETHEREUM_SEPOLIA_PROVIDER`, respectively. Requires only
Python's standard library and a provider supporting JSON-RPC batches. No
transactions are submitted. Network permission can be scoped to this script.

The script checks chain ID, bytecode, game type 1006, and factory registration
before collecting fields. Reads use one L1 block, 20-second request timeouts,
and one snapshot restart if a reorg occurs. Pass `--expected-factory <address>`
when a trusted factory for the requested network/deployment is available from
user context or repository deployment configuration. Otherwise report the
script's provenance limitation; self-reported registration is not authentication
of a canonical World Chain deployment.

On success, present the JSON `fields` as one compact Field / Value table in the
returned order. Include every field, the full game address, network/chain ID,
snapshot block number/hash/UTC timestamp, factory, and bond token. Keep addresses
and hashes complete and copyable. Include `warnings` and `errors` briefly.
The script decodes enums, proof lanes, UTC times, and exact ERC-20 bond amounts.
Do not assume the council verifier is a Safe or that deadline expiry resolves
the game. Parent references can point to the anchor registry; do not recurse.

Exit 2 with `This is not a WIP1006 game contract address.` means return only
that sentence. Exit 1 means verification/RPC failure or an incomplete report,
not evidence of a non-WIP1006 contract. If partial JSON is returned, show the
available fields, mark the report incomplete, and identify failed reads.
Never expose provider URLs or credentials.
