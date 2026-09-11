# SP1 guest crypto patches

This workspace applies the crypto patches used by [OP Succinct at
e0f4902](https://github.com/succinctlabs/op-succinct/blob/e0f49021738627843225fdd07ec843bd5cd8f8be/Cargo.toml#L242)
for the crates present in our guest dependency graph. Cargo only honors patches
at the consuming workspace root; patches in dependencies do not propagate here.
The guest lockfile pins the fork commits. Host and Nitro workspaces are unaffected.

| Patch | Accelerated work |
| --- | --- |
| `sha2` | SHA-256 message expansion and compression use SP1 syscalls. |
| `sha3` | Keccak permutations use SP1 syscalls, including Alloy's Keccak hashing. |
| `k256` | secp256k1 arithmetic uses SP1 operations, accelerating transaction sender recovery and EVM `ecrecover`. Its `ecdsa` dependency also avoids redundant verification after recovery while retaining the curve's low-S check. |
| `p256` | P-256 arithmetic uses SP1 operations for the EVM signature-verification precompile. |

These replace expensive sequences of general-purpose RISC-V instructions with
operations checked by specialized SP1 constraints. They preserve the cryptographic
results; they do not make the work free. CPU cycles and prover gas units (PGU)
measure different costs, so a cycle reduction is not an identical PGU reduction.

OP Succinct also patches `tiny-keccak` and `substrate-bn`, neither of which is in
this guest's current dependency graph. Our Revm BN254 backend uses Arkworks, so a
`substrate-bn` patch would not accelerate it. These patches do not accelerate
Blake2F. Comparing full-block PGU/gas against isolated Blake2F rounds mixes different
workloads: block execution includes call/loop overhead and gas outside Blake2F.

## Validation and rollout

- Check actual patch selection with `cargo tree --locked` in this directory.
- Run `cargo test --locked -p world-chain-proof-succinct-aggregation --test crypto`
  here for hash and signature-recovery vectors against the patched dependencies.
  These are native regression tests, not a measurement of zkVM performance.
- Rebuild using the pinned Docker compiler and run `just update-proof-vkeys`, then
  `just verify-proof-vkeys`, from the repository root. Commit the updated `.sp1`
  measurements alongside guest changes. Both range and aggregation use hashing;
  derive both keys rather than assuming aggregation is unchanged.
- Build matching worker/release artifacts. `MultiProofGame` stores
  `aggregationVKey` and `rangeVKeyCommitment` immutably, so activate a new game
  implementation with the new measurements. Existing games retain their old keys
  and need compatible proof artifacts. Do not reuse old proof sessions as new-key
  proofs. This is a guest identity change, not a change to the SP1 verifier circuit
  or the journal encoding; no Nitro PCR rotation is required by this patch.
- Replay identical saved witnesses with old and new guests, checking equal public
  values and recording CPU cycles, syscall counts, PGU and execution gas. Include
  the original `handleOps` workload. Do not claim a specific PGU/gas improvement
  until this comparison has been run.
