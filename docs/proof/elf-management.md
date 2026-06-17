# SP1 guest ELF management

The World Chain fault-proof system runs two SP1 guest programs:

| Program | Purpose | Crate |
|:---|:---|:---|
| `world-chain-proof-succinct-range-ethereum` | Proves correct execution of a block range | `proofs/succinct/programs/range-ethereum` |
| `world-chain-proof-succinct-aggregation`     | Aggregates many range proofs into one     | `proofs/succinct/programs/aggregation`     |

Both are compiled to RISC-V ELFs by `cargo prove build` (the SP1 toolchain) and are consumed by
the `proof` CLI, the SP1 worker, and the devnet's full-stack tests. They are also referenced on
chain indirectly via the SP1 vkeys — the vkeys are deterministic over the ELF bytes, so the ELF
bytes **are** the governance anchor for the proof lane.

## How the ELFs reach the host binaries

We use the OP Succinct upstream pattern (see [succinctlabs/op-succinct/utils/build](https://github.com/succinctlabs/op-succinct/tree/main/utils/build)):

1. `proofs/succinct/elfs/build.rs` calls
   [`sp1_build::build_program_with_args`](https://docs.rs/sp1-build/latest/sp1_build/fn.build_program_with_args.html)
   for each guest crate at `cargo build` time.
2. `sp1-build` invokes `cargo prove build` against the program crate, producing a deterministic
   RISC-V ELF and emitting a `cargo:rustc-env=SP1_ELF_<package>=<path>` directive for every
   program target it built.
3. `proofs/succinct/elfs/src/lib.rs` calls
   [`sp1_sdk::include_elf!`](https://docs.rs/sp1-sdk/latest/sp1_sdk/macro.include_elf.html)
   which expands to `include_bytes!(env!("SP1_ELF_<package>"))`, embedding the ELF bytes into
   the prover binary at link time via the `world-chain-proof-succinct-elfs` crate.

Net effect: the ELFs are never on disk for the host crate to find — they're statically baked
into every binary that links `world-chain-proof-succinct-elfs` (e.g. `world-chain-sp1-worker`).
There is no committed ELF blob and no manifest of SHA-256s. The `proof` CLI and the devnet
SP1 worker load ELFs at runtime from `RANGE_ELF_PATH` / `AGG_ELF_PATH` env vars via
`EnvSuccinctProver::new`. The on-chain governance anchor is the SP1 vkey computed from the
embedded bytes (`just proof-vkeys`), which is exactly what we register on
`OPSuccinctFaultDisputeGame`.

## Reproducibility

`sp1_build::build_program_with_args` is called with `docker: true` and `tag: "v6.1.0"` by
default, so a `cargo build -p proof --features sp1` from a clean checkout produces bit-for-bit
identical ELFs (and therefore identical vkeys) regardless of host toolchain — `cargo-prove`
runs inside the pinned `succinctlabs/sp1:v6.1.0` image.

Set `SP1_BUILD_DOCKER=false` to switch to a locally-installed `cargo-prove` instead. This is the
mode `Dockerfile.proof` uses internally, because the Docker daemon is not reachable from inside
a `docker build`; the Dockerfile installs the same pinned `sp1up --version v6.1.0` so the
resulting ELFs are still reproducible across hosts.

## Local development

Nothing extra is required:

```bash
cargo build -p proof --features sp1     # builds guest ELFs (first time only) and the host CLI
cargo build -p world-chain-sp1-worker   # likewise
just proof-vkeys                         # prints the on-chain vkey commitments
```

The first build triggers `cargo prove build --docker --tag v6.1.0` for each guest crate (a few
minutes). Subsequent builds reuse the cached ELFs unless the guest source or the SP1 toolchain
tag changes — `sp1-build` calls `cargo:rerun-if-changed` on every dependency of the program
crate, so any meaningful source edit invalidates the cache.

Requirements:

- Docker (default reproducibility mode pulls `succinctlabs/sp1:v6.1.0`), or
- The SP1 toolchain on `PATH` (`curl -L https://sp1.succinct.xyz | bash && sp1up --version v6.1.0`)
  with `SP1_BUILD_DOCKER=false`.

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
new vkeys. Both rotate the on-chain measurements (`range_vkey_commitment` and `aggregation_vkey`
registered on `OPSuccinctFaultDisputeGame`), which is a governance event: the on-chain registries
need a matching update before the new prover can be deployed.

The workflow is just normal source-control:

1. Edit the guest source or bump the SP1 toolchain `tag` in `proofs/succinct/elfs/build.rs`.
2. `cargo build -p proof --features sp1` to confirm the new ELFs build.
3. `just proof-vkeys` to print the new vkey commitments.
4. Mention the rotated vkeys in the PR description and link the matching on-chain registry
   update.

## CI

There is no separate `elf.yml` workflow: the SP1 ELFs are compiled as part of every host
`cargo build` that touches the `sp1` feature. Reproducibility is enforced implicitly — the
`release-proof.yml` workflow rebuilds from source on every release tag and stamps the resulting
vkeys into `manifest.json`. Any drift in the guest source or the toolchain tag shows up as a
vkey diff in the release-notes "measurements" section.

Workflow updates (`elf.yml` removal, `release-proof.yml` simplifications) are listed in the PR
description for a maintainer with `workflows` scope to apply — the agent that opened this PR
cannot write to `.github/workflows/**`.

## Comparison with op-succinct

[succinctlabs/op-succinct](https://github.com/succinctlabs/op-succinct) is the upstream SP1
proof system that World Chain's proof system is based on. It uses exactly the same pattern:
`sp1_build::build_program_with_args` in `build.rs` compiles the guest ELF at host `cargo build`
time, and `sp1_sdk::include_elf!()` embeds it into the host binary. No ELF binaries or SHA-256
hash manifests are committed to source control — the build is fully automatic and reproducible
via the pinned `cargo-prove` toolchain.

World Chain follows this pattern directly:

| Layer | op-succinct | World Chain proof system |
|:---|:---|:---|
| Source-of-truth artifact | SP1 guest ELF | SP1 guest ELF |
| Build reproducibility    | `build_program_with_args` + pinned SP1 toolchain tag | `build_program_with_args` + pinned `tag` in `build.rs` (`docker: true`) |
| On-chain anchor          | SP1 vkey on `OPSuccinctL2OutputOracle` | SP1 vkey on `OPSuccinctFaultDisputeGame` |
| Where the artifact lives | **Embedded into the host binary via `include_elf!()`** | **Embedded into the host binary via `include_elf!()`** |
| Committed ELF / hash file | None | None |

For World Chain's Nitro lane (`proofs/nitro/`), a separate PCR-commit pattern is used for the
TEE enclave image; the SP1 lane follows the op-succinct embed-at-compile-time pattern, which
avoids carrying any ELF artifacts (committed bytes or committed SHA-256s) in source control.

## Files of interest

| Path | Role |
|:---|:---|
| `proofs/succinct/elfs/build.rs` | Invokes `sp1_build::build_program_with_args` for each guest crate |
| `proofs/succinct/elfs/src/lib.rs` | `range_elf()` / `aggregation_elf()` via `include_elf!()` |
| `proofs/succinct/utils/host/src/env_prover.rs` | `range_elf()` / `aggregation_elf()` via runtime `RANGE_ELF_PATH` / `AGG_ELF_PATH` env vars |
| `proofs/succinct/programs/range-ethereum/` | Range guest source |
| `proofs/succinct/programs/aggregation/`    | Aggregation guest source |
| `Dockerfile.proof` | Builder image installs the SP1 toolchain and sets `SP1_BUILD_DOCKER=false` |
| `Justfile` | `just proof-vkeys` prints the current vkey commitments |
| `.github/workflows/release-proof.yml` | Release gate: rebuilds, snapshots vkeys into `manifest.json` |
