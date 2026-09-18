---
name: scan-invalid-games
description: Scan WIP1006 games inside their challenge deadline for root claims that disagree with a supplied L2 consensus RPC at finalized L2 blocks, including challenge status and game creator.
---

Run `python3 scripts/scan_games.py <factory-address> <l2-consensus-rpc> [mainnet|sepolia]`,
resolving the script relative to this skill directory. Execute it directly; reading
or recreating the script is unnecessary unless troubleshooting.

The user supplies the dispute game factory and its corresponding L2 consensus RPC.
Default to mainnet; use sepolia when specified. L1 comes from `ETHEREUM_PROVIDER`
or `ETHEREUM_SEPOLIA_PROVIDER`, respectively, as in invalid-parent. Requires only
Python's standard library and an L1 provider supporting JSON-RPC batches.

The scanner pins reads to one latest L1 block and uses its timestamp. It scans
factory entries backward, skipping non-1006 types, and stops at the first expired
WIP1006 challenge deadline. This assumes WIP1006 challenge deadlines are
nondecreasing with factory index, including across implementation changes.
Only L2 heights at or below `optimism_syncStatus.finalized_l2.number` are compared.
Expected roots come from `optimism_outputAtBlock` on the supplied client; they
are not independently derived or authenticated by this skill.

Inside the original `challengeDeadline()` is the eligibility criterion, even for
already-challenged or resolved games. It does not mean another challenge can be
submitted. `claimData.deadline` is not the original challenge deadline after a
challenge. A nonzero stored challenger address determines whether a game was
challenged. No transactions are submitted.

On success, present the JSON report as a concise mismatch table with game address,
L2 block, creator, challenged flag, game status, deadline, and claimed/expected
roots. Include the L1 snapshot, finalized L2 height, and skipped unfinalized count.
Report no mismatches only after a successful complete scan. RPC or decoding
failures exit nonzero and are not evidence of a clean scan. Do not expose RPC URLs
or credentials in the report.
