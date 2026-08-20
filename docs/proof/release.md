# Prover release process

Prover deployables are released under `proofs/vX.Y.Z` tags, decoupled from node releases
(`vX.Y.Z`, `release.yml`). One tag bundles all programs and services: git sha → SP1 vkeys →
enclave PCRs → `tee_image_id` → image digests, so a single version maps a network deployment
to the entire proof system.

## The registry

[`proof-releases.lock`](../../proof-releases.lock) is the source of truth.
Each `[releases."X.Y.Z"]` entry records the release's measurements (vkeys, ELF hashes, PCRs,
`tee_image_id = keccak256(pcr0)`); `latest_rc` / `latest_stable` point at the current entry.
Entries are append-only and immutable once merged. `scripts/check-proof-releases.py` validates
all of this in CI and locally.

## The measurement gate

A PR cannot change what the proof system measures without committing to a new release:

- [`vkeys.yml`](../../.github/workflows/vkeys.yml) — SP1 vkeys rebuilt from source must match
  the registry's current entry (`just verify-proof-vkeys`).
- [`proof-gate.yml`](../../.github/workflows/proof-gate.yml) — validates the registry itself
  (append-only, `tee_image_id = keccak256(pcr0)`), and when an enclave input changed, rebuilds
  the EIF and requires its PCRs to match the current entry. On drift the job fails and names
  the fix: `just proof-release --version X.Y.Z`.

## Cutting a release

1. Cut the entry, commit, and open a PR (Linux x86_64 + Docker — rebuilds the SP1 vkeys and
   the EIF PCRs from source, appends `[releases."X.Y.Z"]`, and advances `latest_rc`;
   `--stable` for `latest_stable`):

   ```bash
   just proof-release --version X.Y.Z
   ```
2. On merge to main, [`proof-release-tag.yml`](../../.github/workflows/proof-release-tag.yml)
   creates `proofs/vX.Y.Z` and dispatches
   [`release-proof.yml`](../../.github/workflows/release-proof.yml). (Pushing the tag by hand
   triggers the same pipeline.)
3. `release-proof.yml` rebuilds everything from the tag and **fails unless the rebuilt
   measurements match the registry entry**, then publishes images and opens a **draft**
   GitHub release. Review the measurements section, then publish.

A release whose measurements changed is a governance event: activation requires a new game
implementation pinned to the new values, plus PCR approval and enclave key registration when
`tee_image_id` changed. The draft release notes diff measurements against the previous
`proofs/v*` release and flag this.

## What a release produces

| Artifact | Notes |
|:---|:---|
| `manifest.json` | Registry entry (all measurements) + git sha + per-service image digests |
| `world-chain-proof-nitro-enclave.eif` | Enclave image, reproducibly built |
| `ghcr.io/worldcoin/world-chain-<service>:proofs-vX.Y.Z` | All prover service images, built by the same `docker-proof.yml` pipeline as nightly |

## Enclave decoupling

`proofs/backends/nitro/enclave` is a standalone cargo workspace with its own `Cargo.lock`
(excluded from the root workspace), so root `Cargo.lock` churn cannot shift the EIF
measurements. PCRs change only when one of the enclave's actual inputs changes: its own
sources and lockfile, its path deps in `proofs/measured` (`core`, `kona-client`) together
with that workspace's own `[workspace.dependencies]` and lockfile, the
pinned base images / apt snapshot in its Dockerfile, or the pinned nitro-cli version in
`scripts/build-eif.sh`.

## Verifying a release locally

```bash
just verify-proof-vkeys                           # vkeys from source == registry current entry
scripts/build-eif.sh                              # reproduce EIF + PCRs (Linux x86_64 + Docker)
python3 scripts/check-proof-releases.py --current  # what the registry claims
```
