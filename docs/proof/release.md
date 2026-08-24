# Prover release process

Prover deployables are released independently of the node via `proof/vX.Y.Z` tags, handled by
[`.github/workflows/release-proof.yml`](../../.github/workflows/release-proof.yml). Node releases
(`vX.Y.Z` tags, `release.yml`) are unaffected.

## Why a separate tag namespace

A prover release is a governance event whenever its measurements change. The active game
implementation pins both SP1 vkeys and the PCR0 Nitro image ID; Nitro PCR approval separately
controls which image-bound enclave keys may register. A release that changes a measurement requires
a new game implementation, plus PCR approval for a changed Nitro image, before activation. Decoupling the tag
namespaces lets prover releases follow proof-system iteration instead of node/hardfork cadence,
and keeps measurement changes reviewable on their own.

## What a release produces

| Artifact                                                 | Notes |
|:---------------------------------------------------------|:---|
| `manifest.json`                                          | Single source of truth binding git SHA, ELF sha256s, vkeys, PCRs, and image digests |
| `measurements.json`                                      | `.sp1`: range vkey commitment + aggregation vkey + ELF hashes. `.nitro`: PCR0/PCR1/PCR2 of the enclave EIF. Committed at `proofs/measurements.json` |
| `world-chain-proof-nitro-enclave.eif`                    | Enclave image, built reproducibly (see below) |

How the measurements are kept reproducible — the measured workspaces and the Nix enclave
image — is documented in [reproducible-builds.md](./reproducible-builds.md).
| `world-chain-range-ethereum`, `world-chain-aggregation`  | SP1 guest ELFs, rebuilt from source in CI via `sp1_build` (no committed binaries, no hash manifest — see [elf-management.md](./elf-management.md)) |
| `world-chain-proof-<version>-<target>.tar.gz` (+ `.asc`) | GPG-signed `proof` CLI binaries (linux x86_64 / aarch64) |
| `ghcr.io/worldcoin/world-chain-proof:<version>`          | Multi-arch prover image (sp1 + nitro backends, ELFs baked in) |

The draft release notes include a measurements section that diffs the vkeys/PCRs against the
previous `proof/v*` release and flags when a new game implementation or PCR approval is required.

## Same-domain measurement cutovers

Deploy verifier-aware queue routing to the prover-service, defender, and current workers before
activating an implementation with new vkeys or a new Nitro image. Keep the current
workers running as the old generation, add workers built for the new measurements, and only then
activate the new game implementation. Both generations may share one prover-service: workers lease
only games whose pinned aggregation/range vkeys or TEE image id match their own measurements.
SP1 workers derive their vkeys from their embedded guest programs; Nitro workers verify a startup
attestation and derive their TEE image ID from its PCR0, so neither identity is configured manually
on the worker.

Remove the old workers after no matching old-game requests remain. This procedure covers
same-domain implementation rotations. A domain-changing cutover requires a separate policy for
retiring or continuing to defend the old lineage.

## Cutting a release

```bash
git tag proof/v0.1.0 <sha-on-main>
git push origin proof/v0.1.0
```

The workflow gates everything on ELF reproducibility (every `cargo build --features sp1` runs
`sp1_build::build_program_with_args` under the pinned `cargo-prove` toolchain — see
[elf-management.md](./elf-management.md)), then builds all artifacts and opens a **draft**
release for human review. Review the measurements section, then publish.

`workflow_dispatch` runs the same pipeline without creating a release (images are tagged
`dev-<sha>`); use it to validate changes to the pipeline itself.

## Reproducibility requirements

- **SP1 ELFs** are built in the digest-pinned SP1 image and canonical `/root/program` layout,
  then embedded into the host binary at compile time via `sp1_sdk::include_elf!()`. There are no
  committed ELF binaries; their hashes and derived vkeys are committed in the `.sp1` half of
  `proofs/measurements.json`. The
  production worker image consumes the same canonical ELFs and verifies its linked measurements
  before publication. See
  [elf-management.md](./elf-management.md).
- **The enclave EIF** must be bit-for-bit reproducible so anyone can re-derive the game-pinned
  PCRs from source: `flake.nix` builds the rootfs — reproducible by construction, no apt state and
  no build timestamps — and `scripts/build-eif.sh` pins the nitro-cli version that assembles the
  EIF. Bumping either changes the PCRs — expect to approve the new PCR set, register new
  image-bound signers, and activate a game implementation pinned to its PCR0 image ID. See
  [reproducible-builds.md](./reproducible-builds.md).

## Verifying a release locally

```bash
# Reproduce the guest ELFs and on-chain verification keys from source
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
