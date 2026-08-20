# SP1 guest ELF management

The World Chain fault-proof system runs two SP1 guest programs:

| Program | Purpose | Crate |
|:---|:---|:---|
| `world-chain-proof-succinct-range-ethereum` | Proves correct execution of a block range | `proofs/backends/sp1/programs/range-ethereum` |
| `world-chain-proof-succinct-aggregation`     | Aggregates many range proofs into one     | `proofs/backends/sp1/programs/aggregation`     |

Both are compiled to RISC-V ELFs by `cargo prove build` (the SP1 toolchain) and are consumed by
the `world-chain-prover-sp1` CLI, the SP1 worker, and the devnet's full-stack tests. They are also
referenced on chain indirectly via the SP1 vkeys — the vkeys are deterministic over the ELF bytes,
so the ELF bytes **are** the governance anchor for the proof lane.

## How the ELFs reach the host binaries

We use the OP Succinct upstream pattern (see [succinctlabs/op-succinct/utils/build](https://github.com/succinctlabs/op-succinct/tree/main/utils/build)):

1. `proofs/backends/sp1/elfs/build.rs` calls
   [`sp1_build::build_program_with_args`](https://docs.rs/sp1-build/latest/sp1_build/fn.build_program_with_args.html)
   for each guest crate at `cargo build` time.
2. `sp1-build` invokes `cargo prove build` against the program crate, producing a deterministic
   RISC-V ELF and emitting a `cargo:rustc-env=SP1_ELF_<package>=<path>` directive for every
   program target it built.
3. `proofs/backends/sp1/elfs/src/lib.rs` calls
   [`sp1_sdk::include_elf!`](https://docs.rs/sp1-sdk/latest/sp1_sdk/macro.include_elf.html)
   which expands to `include_bytes!(env!("SP1_ELF_<package>"))`, embedding the ELF bytes into
   the prover binary at link time via the `world-chain-proof-sp1-elfs` crate.

Net effect: the ELFs are never on disk for the host crate to find — they're statically baked
into every binary that links `world-chain-proof-sp1-elfs` (e.g. `world-chain-proof-sp1-worker`).
There is no committed ELF blob. The derived vkeys and ELF SHA-256s are recorded in the release
registry (`proof-releases.lock`). The on-chain governance anchor is the SP1 vkey computed from the
embedded bytes (`just proof-vkeys`), which is pinned in the `MultiProofGame` implementation.

## Reproducibility

`sp1_build::build_program_with_args` uses Docker by default with the SP1 v6.3.1 linux/amd64
image pinned by digest. A `cargo build -p world-chain-prover-sp1` from a clean checkout therefore
produces bit-for-bit identical ELFs and vkeys regardless of host toolchain.

Set `SP1_BUILD_DOCKER=false` to switch to a locally-installed `cargo-prove` instead. This is the
non-production development mode: absolute workspace and Cargo registry paths can enter loadable
guest sections and rotate the vkeys even when the Rust source and SP1 version are unchanged.

The production `sp1-worker` target in `Dockerfile.prover` builds the guests in the same pinned
SP1 image and `/root/program` layout as the default local build. The
`world-chain-proof-sp1-guest-builder` binary calls the pinned `sp1-build` library, so the Docker
build and local build share Succinct's compiler flags instead of maintaining a second compilation
recipe. The image then copies those exact ELFs into the host builder and sets
`SP1_SKIP_PROGRAM_BUILD=true`, so the worker embeds them without a second compilation. The image
build fails unless the ELF hashes and vkeys computed from the final worker binary match the
registry's current release entry (`vkeys --check-registry proof-releases.lock`).

## Local development

Nothing extra is required:

```bash
cargo build -p world-chain-prover-sp1   # builds guest ELFs (first time only) and the host CLI
cargo build -p world-chain-proof-sp1-worker   # likewise
just proof-vkeys                         # prints the on-chain vkey commitments
```

The first build runs each guest build inside the digest-pinned SP1 v6.3.1 image (a few minutes).
Subsequent builds reuse the cached ELFs unless the guest source or SP1 image reference changes —
`sp1-build` calls `cargo:rerun-if-changed` on every dependency of the program
crate, so any meaningful source edit invalidates the cache.

Requirements:

- Docker (default reproducibility mode pulls the digest-pinned `succinctlabs/sp1:v6.3.1`), or
- The SP1 toolchain on `PATH` (`curl -L https://sp1.succinct.xyz | bash && sp1up --version v6.3.1`)
  with `SP1_BUILD_DOCKER=false` for non-production iteration only.

Build the production worker with its dedicated target:

```bash
docker build --target sp1-worker \
  --build-arg PROVER_PACKAGE=world-chain-proof-sp1-worker \
  --build-arg PROVER_BIN=world-chain-proof-sp1-worker \
  -f Dockerfile.prover .
```

## Fast iteration

Once the ELFs have been built once they live under
`target/elf-compilation/docker/.../release/`. Set `SP1_SKIP_PROGRAM_BUILD=true` in subsequent
`cargo check` / `cargo clippy` runs to skip the SP1 compile while still letting `include_elf!()`
resolve against the cached ELFs.

Skipping the build entirely (no cached ELF) makes `include_elf!()` fail at compile time with a
"could not find environment variable" or "couldn't open file" error — there is no fallback,
because the design choice is to refuse to link a host binary against an absent guest.

## What changed when SP1 programs are updated

A change to the guest source or a bump of the pinned `tag` produces new ELF bytes and therefore
new vkeys. Both rotate the on-chain measurements (`range_vkey_commitment` and `aggregation_vkey`)
pinned in the `MultiProofGame` implementation, which is a governance event: a new game
implementation must be deployed and activated before the new prover is used.

The workflow is just normal source-control:

1. Edit the guest source or bump the matching SP1 image reference in
   `proofs/backends/sp1/elfs/build.rs` and `Dockerfile.prover`.
2. `cargo build -p world-chain-prover-sp1` to confirm the new ELFs build.
3. `just proof-vkeys` to print the new vkey commitments.
4. Mention the rotated vkeys in the PR description and link the matching game-implementation
   deployment.

## CI

The `vkeys.yml` workflow recomputes the manifest through the canonical Docker path. The
`docker-proof.yml` SP1 worker job uses the dedicated `sp1-worker` target and runs
`world-chain-proof-sp1-worker vkeys --check-registry` against the linked binary before publishing it.

## Comparison with op-succinct

[succinctlabs/op-succinct](https://github.com/succinctlabs/op-succinct) is the upstream SP1
proof system that World Chain's proof system is based on. It uses exactly the same pattern:
`sp1_build::build_program_with_args` in `build.rs` compiles the guest ELF at host `cargo build`
time, and `sp1_sdk::include_elf!()` embeds it into the host binary. No ELF binaries are committed
to source control; the derived vkeys and hashes in the release registry can be reproduced with the
pinned `cargo-prove` toolchain.

World Chain follows this pattern directly:

| Layer | op-succinct | World Chain proof system |
|:---|:---|:---|
| Source-of-truth artifact | SP1 guest ELF | SP1 guest ELF |
| Build reproducibility    | `build_program_with_args` + pinned SP1 toolchain tag | Digest-pinned SP1 image and canonical workspace layout |
| On-chain anchor          | SP1 vkey on `OPSuccinctL2OutputOracle` | SP1 vkeys pinned in `MultiProofGame` |
| Where the artifact lives | **Embedded into the host binary via `include_elf!()`** | **Embedded into the host binary via `include_elf!()`** |
| Committed ELF blob       | None | None |

For World Chain's Nitro lane (`proofs/backends/nitro/`), a separate PCR-commit pattern is used for the
TEE enclave image; the SP1 lane follows the op-succinct embed-at-compile-time pattern, which
avoids carrying any ELF artifacts (committed bytes or committed SHA-256s) in source control.

## Files of interest

| Path | Role |
|:---|:---|
| `proofs/backends/sp1/elfs/build.rs` | Invokes `sp1_build::build_program_with_args` for each guest crate |
| `proofs/backends/sp1/guest-builder` | Invokes the same pinned `sp1-build` compiler recipe inside the production guest-builder image |
| `proofs/backends/sp1/elfs/src/lib.rs` | `range_elf()` / `aggregation_elf()` via `include_elf!()` |
| `proof-releases.lock` | Derived SP1 vkeys and ELF SHA-256s, per release |
| `proofs/backends/sp1/host/src/*_prover.rs` | CPU, mock, and network provers over the embedded ELFs |
| `proofs/backends/sp1/programs/range-ethereum/` | Range guest source |
| `proofs/backends/sp1/programs/aggregation/`    | Aggregation guest source |
| `Dockerfile.prover` | Builds canonical guests once and embeds them unchanged in the SP1 worker |
| `Justfile` | `just proof-vkeys` prints the current vkey commitments |
| `.github/workflows/release-proof.yml` | Release gate: rebuilds, snapshots vkeys into `manifest.json` |
