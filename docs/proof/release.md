# Prover release process

Prover deployables are released independently of the node via `proof/vX.Y.Z` tags, handled by
[`.github/workflows/release-proof.yml`](../../.github/workflows/release-proof.yml). Node releases
(`vX.Y.Z` tags, `release.yml`) are unaffected.

## Why a separate tag namespace

A prover release is a governance event whenever its measurements change: the SP1 vkeys and the
Nitro enclave PCRs are registered on-chain (per WIP-1006's proof-lane registries), and a release
that changes them requires a registry update before it can be deployed. Decoupling the tag
namespaces lets prover releases follow proof-system iteration instead of node/hardfork cadence,
and keeps measurement changes reviewable on their own.

## What a release produces

| Artifact | Notes |
|:---|:---|
| `manifest.json` | Single source of truth binding git SHA, ELF sha256s, vkeys, PCRs, and image digests |
| `vkeys.json` | Range vkey commitment + aggregation vkey, computed from the committed ELFs |
| `pcrs.json` | PCR0/PCR1/PCR2 of the enclave EIF |
| `world-chain-nitro-enclave.eif` | Enclave image, built reproducibly (see below) |
| `world-chain-range-ethereum`, `world-chain-aggregation` | The committed SP1 guest ELFs |
| `world-chain-proof-<version>-<target>.tar.gz` (+ `.asc`) | GPG-signed `proof` CLI binaries (linux x86_64 / aarch64) |
| `ghcr.io/worldcoin/world-chain-proof:<version>` | Multi-arch prover image (sp1 + nitro backends, ELFs baked in) |

The draft release notes include a measurements section that diffs the vkeys/PCRs against the
previous `proof/v*` release and flags when an on-chain registry update is required.

## Cutting a release

```bash
git tag proof/v0.1.0 <sha-on-main>
git push origin proof/v0.1.0
```

The workflow gates everything on ELF reproducibility (`just build-proof-elfs` must reproduce the
committed ELFs bit-for-bit), then builds all artifacts and opens a **draft** release for human
review. Review the measurements section, then publish.

`workflow_dispatch` runs the same pipeline without creating a release (images are tagged
`dev-<sha>`); use it to validate changes to the pipeline itself.

## Reproducibility requirements

- **SP1 ELFs** are built with `cargo prove build --docker` at a pinned SP1 toolchain tag and
  committed under `proofs/succinct/elf/`. CI (`elf.yml` and the release gate) rebuilds and diffs
  them.
- **The enclave EIF** must be bit-for-bit reproducible so anyone can re-derive the registered
  PCRs from source: `proofs/nitro/Dockerfile` pins base images by digest and apt packages to a
  fixed snapshot.debian.org timestamp, and `scripts/build-eif.sh` pins the nitro-cli version that
  assembles the EIF. Bumping any of these pins changes the PCRs — expect to re-register them.

## Verifying a release locally

```bash
# Reproduce the guest ELFs and vkeys
just build-proof-elfs
just proof-vkeys

# Reproduce the enclave EIF and PCRs (Linux x86_64 + Docker)
scripts/build-eif.sh
```

Compare the output against the release's `manifest.json`.

## Adding a prover binary to the release

When a new prover deployable lands on `main` (e.g. the `sp1-worker`):

1. Add a build/merge job pair in `release-proof.yml`, passing
   `PROVER_PACKAGE`/`PROVER_BIN`/`FEATURES` build args to `Dockerfile.proof` and a unique
   `digest_artifact_prefix`.
2. Add a matrix entry to the `build-binaries` job for the signed tarball.
3. Record the new image digest in the manifest step.
