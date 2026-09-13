# World Chain Defender

The defender reconstructs the same selected game lineage as the proposer from the current
`AnchorStateRegistry` checkpoint. For each finalized L2 interval it computes the expected output
root, looks up the highest sequential attempt through the stock `DisputeGameFactory`, and stops at
the first missing or invalidated transition.

For selected proofless games it submits a TEE proof. If a selected game is challenged, it drives
the configured independent proof lanes until the contract threshold is reached. The proposer owns
resolution, retry creation, and anchor advancement.

Only asynchronous proof progress is retained between ticks. Each tick reconstructs the lineage and
drops proof workflows for games that are no longer selected. A proof-timeout retry therefore moves
the defender to the replacement attempt. Descendants of the invalidated attempt need no further
proof support: the proposer bond manager resolves them as `INVALID_PARENT` and claims their refunds.

## Private proof submission

Set `L1_SUBMISSION_RPC_URL` to a private relay RPC for proof estimation and submission,
with no public fallback. Reads and receipt checks use the existing L1 RPCs.
If unset, submissions use the normal L1 RPC.

Private submission mitigates reward theft. Residual relay/builder trust and reorg risks
are acknowledged and accepted.

After a submission attempt, a different accepted lane recipient emits
`proof_reward_recipient_mismatch` (possible frontrun or competing prover). Checks retry on RPC
errors and run before game cleanup; tracking lasts for the current process and ends after the
recipient is checked. This is not a persistent reorg monitor.
