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
