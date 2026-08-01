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
the defender to the replacement attempt; resolving and recovering the abandoned lineage is a manual
operator responsibility.
