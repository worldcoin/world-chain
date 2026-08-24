# Reproducible proof builds

The proof system's trust anchors are measurements: the SP1 guest **vkeys** and the Nitro
enclave's **PCR0/PCR1/PCR2**. Both are hashes of compiled artifacts, and both are approved
on-chain. If a build is not reproducible, nobody can re-derive those values from source —
and a measurement that drifts becomes indistinguishable from build nondeterminism.

Two mechanisms keep them stable.

## 1. Measured crates are their own workspaces

A crate is **measured** if it is compiled into a guest ELF or into the enclave binary. Every
measured crate is excluded from the root workspace and carries its own `Cargo.lock`:

| Path | Compiled into |
|:---|:---|
| `proofs/core` | both the SP1 guests and the enclave |
| `proofs/kona/client` | both |
| `proofs/backends/nitro/enclave` | the enclave binary (EIF → PCR0/PCR2) |
| `proofs/backends/sp1/programs` | the guest ELFs → vkeys |

Each pins its own `[workspace.dependencies]` rather than inheriting the root workspace's.
That is the whole point: sharing the root's dependency table means a version bump made for
the node silently rotates an on-chain trust anchor.

> **Adding or bumping a dependency in any of these crates is a measurement change.** Expect
> new PCRs or new vkeys, and expect to re-register them.

Two consequences worth knowing:

- `cargo build` at the repo root does **not** build these crates as workspace members. Use
  `--manifest-path`, e.g. `cargo test --manifest-path proofs/core/Cargo.toml`.
- Host-side code must not leak into a measured crate. On-chain registration lives in
  `proofs/backends/nitro/register` (a root-workspace crate) precisely so `alloy-provider`,
  the transaction signer and their transitive graph stay out of the enclave's lockfile.

## 2. The enclave image is built with Nix

`flake.nix` builds the rootfs that `nitro-cli` measures into PCR0 and PCR2. It is
reproducible by construction: there is no `apt`, so no dpkg database, no apt logs and no
ldconfig aux-cache embedding inode numbers; every path is a content-addressed store path; and
`dockerTools` stamps a fixed creation time instead of "now".

### Prerequisites

- **Nix with flakes.** Determinate Nix enables them by default.
- **An `x86_64-linux` builder.** EIFs are amd64-only. On a Linux x86_64 host this is just the
  local machine. On macOS you need a remote builder or `nix.linux-builder`; evaluation works
  anywhere, building does not.

### Build the image

```bash
nix build .#enclave-image      # -> ./result, a docker archive
docker load -i ./result
```

### Build the EIF and read the PCRs

`scripts/build-eif.sh` does the image build, the `nitro-cli` build and the conversion:

```bash
scripts/build-eif.sh target/eif
cat target/eif/pcrs.json
```

There is no Dockerfile fallback. There used to be one, and it built a Debian-based rootfs —
a completely different filesystem from the Nix one, and therefore different PCRs. Keeping it
meant the published enclave image and the measured enclave image could disagree, which makes
a PCR impossible to trust. The script now fails if Nix is missing rather than quietly
measuring something else.

### Development shell

```bash
nix develop
```

Gives you the toolchain from `rust-toolchain.toml` plus `clang`, `cmake`, `pkg-config`,
`openssl` and `just`. Available for Linux and macOS, on both architectures.

## What Nix does and does not cover

Covered — the enclave rootfs, and therefore **PCR0** and **PCR2**.

Not covered:

- **PCR1** is the kernel and bootstrap ramdisk, which come from prebuilt blobs shipped in
  `aws-nitro-enclaves-cli`. Pinning the nitro-cli version pins PCR1; the blobs themselves are
  vendored binaries, not built from source.
- **The SP1 guest ELFs** are already reproducible a different way — `sp1_build` compiles them
  in the SP1 toolchain image pinned by digest in `proofs/backends/sp1/elfs/build.rs`. Nix is
  not involved and does not need to be.
- **The EIF assembly** is `nitro-cli build-enclave`'s job. Nix makes its input deterministic;
  turning that rootfs into an EIF is deterministic given a deterministic rootfs.

## When a measurement changes

1. Rebuild and read the new values: `scripts/build-eif.sh target/eif`.
2. Confirm the change was intended. A measurement moving without a deliberate change to a
   measured crate means something leaked into the measured graph — find it before shipping.
3. Re-register: new PCRs need on-chain approval, and a new `tee_image_id` needs the enclave
   key registered again. See [release.md](./release.md).

## Verifying reproducibility

Build the image twice and compare. With Nix the store path is the answer:

```bash
nix build .#enclave-image --rebuild
```

`--rebuild` builds again and fails if the result differs from what is already in the store.
