# Reproducible proof builds

The proof system's trust anchors are measurements: the SP1 guest **vkeys** and the Nitro
enclave's **PCR0/PCR1/PCR2**. Both are hashes of compiled artifacts, and both are approved
on-chain. If a build is not reproducible, nobody can re-derive those values from source —
and a measurement that drifts becomes indistinguishable from build nondeterminism.

Two mechanisms keep them stable.

## 1. Measured crates are their own workspaces

A crate is **measured** if it is compiled into a guest ELF or into the enclave binary. All
measured crates live under `proofs/measured/`, excluded from the root workspace. A `Cargo.lock`
exists per measured **artifact** — the shared libraries have none; their versions resolve
inside each consumer artifact's lockfile:

| Path | Compiled into | Lockfile |
|:---|:---|:---|
| `proofs/measured/core` | both the SP1 guests and the enclave | none — resolved per artifact |
| `proofs/measured/kona-client` | both | none — resolved per artifact |
| `proofs/measured/nitro-enclave` | the enclave binary (EIF → PCR0/PCR2) | own `Cargo.lock` → PCRs |
| `proofs/measured/sp1-programs` | the guest ELFs → vkeys | own `Cargo.lock` → vkeys |

Each pins its own `[workspace.dependencies]` rather than inheriting the root workspace's.
That is the whole point: sharing the root's dependency table means a version bump made for
the node silently rotates an on-chain trust anchor.

> **Adding or bumping a dependency in any of these crates is a measurement change.** Expect
> new PCRs or new vkeys, and expect to re-register them.

Two consequences worth knowing:

- `cargo build` at the repo root does **not** build these crates as workspace members. Use
  `--manifest-path`, e.g. `cargo test --manifest-path proofs/measured/core/Cargo.toml`.
- Host-side code must not leak into a measured crate. On-chain registration lives in
  `proofs/backends/nitro/register` (a root-workspace crate) precisely so `alloy-provider`,
  the transaction signer and their transitive graph stay out of the enclave's lockfile.

## 2. The EIF is built with Nix, end to end

`flake.nix` builds the enclave binary, its rootfs, and the EIF itself. The assembly is
[monzo/aws-nitro-util](https://github.com/monzo/aws-nitro-util): deterministic cpio ramdisks
(sorted entries, epoch mtimes, root-owned) fed to AWS's own `eif_build`, using the same
AWS-published kernel/init/nsm.ko blobs `nitro-cli` ships. No Docker daemon or linuxkit is
involved anywhere in the measured path.

That last part is load-bearing. The previous pipeline converted the Nix rootfs to an EIF via
`docker load` + `nitro-cli build-enclave` (linuxkit), and the resulting PCR0/PCR2 depended on
the machine doing the conversion — three builds of the same commit produced three PCR sets.
Three things fixed it:

- **EIF assembly moved into Nix** (above), so the ramdisks are a pure function of the flake
  inputs rather than of a container daemon's export behavior.
- **A constant build path.** Cargo hashes the absolute workspace path into every crate's
  `-Cmetadata`, so the same source built in two directories produces two binaries. Sandboxed
  Nix builds all run in `/build`; sandbox-less builders (the CI pods, which cannot use user
  namespaces) get a random per-build directory, so `flake.nix` relocates the build to
  `/build` when it exists and the CI workflows pre-create it.
- **`trim-paths` in the enclave's release profile**, so panic locations stop embedding
  absolute source paths — defense in depth for any builder where neither the sandbox nor
  `/build` applies.

### Prerequisites

- **Nix with flakes.** Determinate Nix enables them by default.
- **An `x86_64-linux` builder.** EIFs are amd64-only. On a Linux x86_64 host this is just the
  local machine. On macOS you need a remote builder or `nix.linux-builder`; evaluation works
  anywhere, building does not.

### Build the EIF and read the PCRs

```bash
nix build .#eif                # -> ./result/image.eif + ./result/pcr.json
```

Or via the script, which also emits a comparable `measurements.json`:

```bash
scripts/build-eif.sh target/eif
diff <(jq -S . proofs/measurements.json) <(jq -S . target/eif/measurements.json)
```

It writes a whole `measurements.json` — the committed file with the freshly measured PCRs
substituted into `.nitro` — so the diff above answers "does this checkout still measure to
what it claims" directly.

There is no Dockerfile fallback. There used to be one, and it built a Debian-based rootfs —
a completely different filesystem from the Nix one, and therefore different PCRs. Keeping it
meant the published enclave image and the measured enclave image could disagree, which makes
a PCR impossible to trust. The script now fails if Nix is missing rather than quietly
measuring something else.

The OCI image (`nix build .#enclave-image`) still exists and is published with releases, but
it is provenance and a local-run convenience — nothing measured is derived from it.

### Development shell

```bash
nix develop
```

Gives you the toolchain from `rust-toolchain.toml` plus `clang`, `cmake`, `pkg-config`,
`openssl` and `just`. Available for Linux and macOS, on both architectures.

## What Nix does and does not cover

Covered — the enclave binary, the rootfs, the ramdisks and the EIF assembly, and therefore
**PCR0**, **PCR1** and **PCR2**.

Not covered:

- **The kernel, init and nsm.ko** inside the EIF are AWS's prebuilt blobs (the same ones
  `nitro-cli` ships), pinned through the `nitro-util` flake input. They are vendored
  binaries, not built from source; pinning the input pins their bytes and so their PCRs.
- **The SP1 guest ELFs** are already reproducible a different way — `sp1_build` compiles them
  in the SP1 toolchain image pinned by digest in `proofs/backends/sp1/elfs/build.rs`. Nix is
  not involved and does not need to be.

## When a measurement changes

1. Rebuild and read the new values: `scripts/build-eif.sh target/eif`.
2. Confirm the change was intended. A measurement moving without a deliberate change to a
   measured crate means something leaked into the measured graph — find it before shipping.
3. Re-register: new PCRs need on-chain approval, and a new `tee_image_id` needs the enclave
   key registered again. See [release.md](./release.md).

## Verifying reproducibility

Build the EIF twice and compare:

```bash
nix build .#eif --rebuild
```

`--rebuild` builds again and fails if the result differs from what is already in the store.
That catches same-machine nondeterminism; cross-machine agreement is what CI's
verify-measurements job checks on every PR that touches a measured input.
