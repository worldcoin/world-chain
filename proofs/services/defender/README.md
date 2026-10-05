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

## Recovering from an unusable anchor

Restart with the same `--recovery-parent <P>` (or `RECOVERY_PARENT=<P>`) as the proposer.
The defender then proves the replacement lineage selected from P in the active domain.
Both services validate P and its canonical finalized root, and resume normal selection once
the ASR has a claim-valid anchor above P. Remove the flag after recovery.
See the [proposer recovery procedure](../proposer/README.md#recovering-from-an-unusable-anchor)
for governance prerequisites and onchain limitations. `lineage.recovery_active` reports
whether recovery selection is active.

## Private proof submission

Set `L1_SUBMISSION_RPC_URL` to a private relay RPC for proof estimation and submission,
with no public fallback. Reads and receipt checks use the existing L1 RPCs.
If unset, submissions use the normal L1 RPC.

Private submission mitigates reward theft. Residual relay/builder trust and reorg risks
are acknowledged and accepted.

Already-proven lanes are skipped before submission. Subsequent failures emit
`proof_submission_failed` for investigation. Inclusion and confirmations each may block up to
`L1_TX_RECEIPT_TIMEOUT_SECONDS` (default 300); inclusion timeouts retry on the next tick.
`proof_submission.inclusion_seconds` measures time to first observed inclusion, excluding confirmations.
