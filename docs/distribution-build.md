# Docker build instructions

This is the build-reference draft for the distribution work. Existing World
Chain and per-component OP Docker recipes are listed below. The coordinated
OP-only/combined image assembler and bundle exports are tracked in BLO-1253;
their build commands must be added and tested before the new release is published.
Do not infer that the proposed image tags already exist from the release preview.

## Inputs and prerequisites

Use Docker with Buildx, enough disk/RAM for the Rust and Go builds, and clean
source trees at the exact World Chain and maintained Optimism commits recorded in
the release manifest. For a source ZIP, extract its `world-chain/` and `optimism/`
directories as siblings. For Git checkouts, use the fork in `optimism-worldchain`,
not the upstream reference checkout; initialize the required pinned submodules.
Do not replace those pins with latest branches.

Build natively for `linux/amd64` or `linux/arm64` where possible. A local `--load`
build below selects one platform; the release pipeline assembles both platforms.
Public dependency downloads and container registry access are required. Go/Rust
toolchains are in the builder stages, not required on the runtime host.

## Existing World Chain image

From the selected World Chain source tree:

```sh
docker buildx build --platform linux/amd64 --load \
  --file Dockerfile --tag world-chain:local .
docker run --rm --entrypoint /usr/local/bin/world-chain world-chain:local --version
```

The current runtime recipe downloads an x86_64 AWS CLI unconditionally. The
distribution implementation must fix and test that before documenting an arm64
build as qualified. Preserve its CA bundle settings and separate cast version.

## Existing OP component recipes from our fork

From the selected maintained Optimism source tree, with the selected op-node
Worldchain version in `OP_NODE_VERSION`:

```sh
docker buildx build --platform linux/amd64 --load \
  --file ops/docker/op-stack-go/Dockerfile --target op-node-target \
  --build-arg "OP_NODE_VERSION=${OP_NODE_VERSION:?Set the selected OP component version}" \
  --build-arg "GIT_COMMIT=$(git rev-parse HEAD)" \
  --build-arg "GIT_DATE=$(git show -s --format=%ct HEAD)" \
  --tag op-node:local .
docker run --rm --entrypoint /usr/local/bin/op-node op-node:local --version
```

For an exported source ZIP, use the fork commit and commit time from the release
metadata instead of the two `git` substitutions. Each component's version comes
from its own upstream base, not the bundle version.

| Component | Existing Docker target | Version build argument |
| --- | --- | --- |
| op-node | `op-node-target` | `OP_NODE_VERSION` |
| op-batcher | `op-batcher-target` | `OP_BATCHER_VERSION` |
| op-proposer | `op-proposer-target` | `OP_PROPOSER_VERSION` |
| op-conductor | `op-conductor-target` | `OP_CONDUCTOR_VERSION` |
| op-challenger and proof helpers | `op-challenger-target` | `OP_CHALLENGER_VERSION`, `CANNON_VERSION` |
| op-deployer and Forge | `op-deployer-target` | `OP_DEPLOYER_VERSION` |
| op-dispute-mon | `op-dispute-mon-target` | `OP_DISPUTE_MON_VERSION` |

These component images are build inputs/reference recipes, not the proposed
aggregate OP-only image. Challenger historical image imports and Kona build
inputs need their own provenance; setting one version argument does not qualify
all embedded helper versions. Preserve Forge's selected pin and verify its trust
behavior against the World Chain baseline.

## Coordinated release builds

### Binary payload assembly

The binary assembler runs on a POSIX host with Python 3.9 or later and consumes
prebuilt payloads exported by qualified build jobs. The producer must finish its
export and keep that tree immutable while assembly runs. The assembler snapshots
the files once and reuses those bytes for all three bundle
variants and staged image inputs:

```text
payloads/
  linux-amd64/
    bin/             # exactly the 12 executables in the distribution plan
    licenses/        # nonempty license/notice files; nested directories allowed
    README.md        # qualified host/runtime-library and installation instructions
  linux-arm64/
    bin/
    licenses/
    README.md
```

Use nonempty regular files, without symlinks. Keep license path components to
ASCII letters/digits plus `.`, `_`, `+` and `-`, starting with a letter/digit.
Each component is limited to 128 characters, each license path to 240, and each
platform to 10,000 files. Extra payload files and missing executables are errors.
This export layout deliberately excludes source worktrees and credentials.

```sh
python3 .github/scripts/distribution-package.py \
  --inputs /path/to/distribution-inputs.json \
  --payload-root /path/to/payloads \
  --output /path/to/new-binary-assembly \
  --source-date-epoch "$WORLD_CHAIN_COMMIT_TIMESTAMP"
```

The output directory must not exist and must be outside the input tree. The
assembler produces six `.tar.gz` bundles, `SHA256SUMS`, `binary-manifest.json`,
and `payloads/linux-*/` snapshots for later image assembly. Executables remain at
the archive root; README, licenses and recorded distribution inputs accompany
them. Files use fixed owner/mode/timestamp metadata. With identical inputs,
source-date epoch and packaging toolchain, archive bytes are reproducible; Python
and zlib versions are recorded in the intermediate manifest.

The intermediate manifest records hashes of the actual snapshot files and
archives. The assembler checks ELF header/CPU compatibility, rejects special
files and detects files changing during a read. These checks do not prove source
provenance, complete license coverage, executable behavior or runtime ABI/library
compatibility. The trusted producer and qualification jobs must establish those.
The input source identities are recorded assertions, not inferred from binaries.

On failure the command exits nonzero and retains private partial output with an
`INCOMPLETE` marker. Do not upload or reuse that output as a completed assembly.
Success requires exit zero, no marker, and the completed intermediate manifest.
Existing output is never overwritten. Inspect/remove a failed output explicitly
before retrying with that path.

This is a binary assembly stage, not a complete GitHub release. Source/examples
ZIPs, signed checksums, published image digests and the final external
`release-manifest.json` are finalized by the subsequent release pipeline.

### Image and release integration

The final assembler must accept the validated distribution inputs, verify source
identities, and export one built payload per platform. Reuse those files for the
World Chain-only, OP-only and combined variants and corresponding binary bundles.
The OP-only image defaults to `op-node`; World Chain and combined images default
to `world-chain`. All executables live in `/usr/local/bin`.

Complete this section with the tested assembler commands, exact output paths,
source ZIP creation, Compose/systemd checks and verification commands as part of
BLO-1253. Resolve all external image/material digests and publish a final external
receipt after generating the images and archives. Release-page layout approval
and the existing release environment approval are separate prerequisites for
publication.
