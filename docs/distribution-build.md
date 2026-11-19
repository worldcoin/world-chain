# Docker build instructions

This reference covers source builds and assembly of three image variants from
one platform payload. Payload production, source/example exports and final
release qualification are tracked in BLO-1253. Local image builds do not publish
the proposed tags or qualify a deployment.

## Inputs and prerequisites

Use Docker with Buildx, enough disk/RAM for the Rust and Go builds, and clean
source trees at the exact World Chain and maintained Optimism commits recorded in
the release manifest. For a source ZIP, extract its `world-chain/` and `optimism/`
directories as siblings. For Git checkouts, use the fork in `optimism-worldchain`,
not the upstream reference checkout; initialize the required pinned submodules.
Schema-2 component `build_commit` values select that component's exact source
tree; do not build every executable from the common fork commit when those
values differ. Source ZIPs include those additional trees under
`component-sources/<component>/`. Check their identities and select each tree
explicitly before using its component build recipe.
Do not replace those pins with latest branches.

Build natively for `linux/amd64` or `linux/arm64` where possible. A local `--load`
build below selects one platform; the release pipeline assembles both platforms.
Public dependency downloads and container registry access are required. Go/Rust
toolchains are in the builder stages, not required on the runtime host.

## Existing World Chain image

From the selected World Chain source tree:

```sh
docker buildx build --platform linux/amd64 --load \
  --build-arg "VERGEN_GIT_SHA=${WORLD_CHAIN_COMMIT:?Set the recorded full source commit}" \
  --file Dockerfile --tag world-chain:local .
docker run --rm --entrypoint /usr/local/bin/world-chain world-chain:local --version
```

The runtime selects the AWS CLI installer for `linux/amd64` or `linux/arm64`;
other architectures fail explicitly. Its reusable `runtime-tools` stage retains
the system utilities, Debian CA version check and `AWS_CA_BUNDLE` setting. The
nightly, release and alphanet workflows exclude `runtime-tools` and `runtime` from caching so these
checks run on each build. The separate `cast` version remains in the final image.

To check runtime tools without compiling the node, build the selected platform:

```sh
docker buildx build --platform linux/arm64 --target runtime-tools --load \
  --no-cache-filter runtime-tools \
  --file Dockerfile --tag world-chain-runtime-tools:local .
docker run --rm --platform linux/arm64 world-chain-runtime-tools:local aws --version
```

Repeat with `linux/amd64`. This checks the tools stage, not the final World Chain
binary, `cast`, OP helpers or combined-image compatibility. Full runtime and
native architecture qualification still use the final release artifacts.

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

For an untagged current Cannon baseline, use schema 2's null `upstream_tag` and
explicit build commit, set `CANNON_VERSION` to the recorded
`v0.0.0-worldchain[.N]`, and check its reported version. Historical Cannon VM
versions and import digests remain separate build materials; they do not assign
the current executable's version.

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

### Image assembly

Use a completed binary assembly's `payloads/linux-amd64` or
`payloads/linux-arm64` directory as the `qualified-payload` named build context.
The Docker targets copy the selected executable subset from that snapshot;
they do not rebuild the node or OP components. The source and version inputs
and license files accompany each image. Final image digests belong in the
external release receipt, avoiding a receipt that contains its own image hash.

From the selected World Chain source directory:

```sh
platform=linux/amd64
payload=/absolute/path/to/new-binary-assembly/payloads/linux-amd64
docker buildx build --platform "$platform" --load \
  --target distribution-combined \
  --build-context "qualified-payload=$payload" \
  --no-cache-filter runtime-tools,distribution-op \
  --file Dockerfile --tag world-chain:combined-local .
docker run --rm --network none world-chain:combined-local --version
docker run --rm --network none --entrypoint /usr/local/bin/op-node \
  world-chain:combined-local --version
```

| Variant | Docker target | Local tag | Default executable |
| --- | --- | --- | --- |
| Combined | `distribution-combined` | `world-chain:combined-local` | `world-chain` |
| World Chain | `distribution-world-chain` | `world-chain:local` | `world-chain` |
| OP | `distribution-op` | `world-chain:op-local` | `op-node` |

Repeat the build with each target and tag. For arm64, select `linux/arm64` and
the matching `payloads/linux-arm64` directory. Do not relabel an amd64 payload
as arm64. The targets check ELF class, endianness and CPU before their version
probes. The empty fallback context makes a missing `qualified-payload` fail
without pulling an image of that name. The final/default `runtime` target
continues to build the existing World Chain image from source.

The three distribution targets default to UID/GID `10001:10001`, with
`HOME=/home/worldchain` and a writable `/app` working directory. Match mounted
data/config ownership to that identity; Kubernetes deployments can use the
existing `runAsUser`, `runAsGroup` and `fsGroup` settings. A read-only root also
requires writable mounts for the selected data directory and temporary storage.
The existing source-built `runtime` target retains its deployment contract;
operators should continue setting its non-root user in the runtime configuration.

The OP and combined variants copy a pinned, patched Alpine musl loader for
historical Cannon embeds; Bookworm's musl package lacks current security fixes.
They install the C++ and udev libraries required by Kona/Foundry from Debian.
Cannon extracts and executes its embedded helpers in `TMPDIR`: provide writable,
executable temporary storage even when the container root is read-only. For
example, a bounded `--tmpfs /tmp:rw,exec,nosuid,size=256m` supports the retained
state-fixture checks. Size production temporary storage for the actual workload.
Native arm64 and amd64 qualification must exercise all supported Cannon state
versions, not just the top-level binary's `--version`.

Before publication, compare the installed executable hashes with the binary
assembly receipt on both platforms, check entrypoints and exact variant subsets,
run each service/helper and retain all native runtime/library versions. The
assembler's ELF checks and Docker version probes do not prove correct chain
execution, source provenance or complete license coverage. Complete source/example
ZIP creation, Compose/systemd and builder/signer qualification under BLO-1253,
then finalize the external receipt with resolved materials and image digests.
Release-page layout approval and the release environment approval remain
separate prerequisites for publication.
