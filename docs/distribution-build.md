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

### Go services from a source ZIP

Use the selected source root for each service. The qualified v2.9.0 Go
programs use Go 1.26.11. The older pinned OP component Dockerfiles select
different Go versions, so use the existing source recipes with the qualified
compiler for source ZIP builds of `op-node`, `op-batcher`, `op-proposer`,
`op-conductor`, `op-challenger` and `op-dispute-mon`.

Start in the directory where you extracted the source ZIP, outside an existing
Git checkout. Install Mise, jq, Bash and Info-ZIP. The exported OP source
includes the pinned superchain registry submodule, and the sync script checks
the generated registry bundle against its committed SHA-256.

```sh
set -eu
component=op-node
arch=arm64 # or amd64
metadata=release-metadata/sources.json
commit=$(jq -er --arg c "$component" '.inputs.components[$c].build_commit // .inputs.sources.optimism' "$metadata")
version=$(jq -er --arg c "$component" '.inputs.components[$c].version' "$metadata")
common=$(jq -er '.inputs.sources.optimism' "$metadata")
source_root=optimism
if [ "$commit" != "$common" ]; then
  source_root="component-sources/$component"
fi
commit_date=$(jq -er --arg p "$source_root" --arg c "$commit" \
  '.sources[] | select(.prefix == $p and .commit == $c) | .commit_timestamp' "$metadata")

(
  cd "$source_root"
  export GOTOOLCHAIN=go1.26.11 GOFLAGS="-mod=readonly -p=4" TZ=UTC
  mise x yq@4.53.6 -- bash op-core/superchain/bundle/sync-superchain.sh
  export GOOS=linux GOARCH="$arch" CGO_ENABLED=0
  output="$component/bin/$component"
  if [ "$component" = op-node ]; then
    TARGETOS=linux TARGETARCH="$arch" \
      GITCOMMIT="$commit" GITDATE="$commit_date" VERSION="$version" \
      mise x just@1.46.0 go@1.26.11 -- just --justfile "$component/justfile" "$component"
  else
    package="github.com/ethereum-optimism/optimism/$component/version"
    case "$component" in
      op-batcher|op-proposer|op-conductor) version_symbol=main.Version ;;
      op-challenger|op-dispute-mon) version_symbol="$package.Version" ;;
      *) echo "Unsupported direct Go service: $component" >&2; exit 1 ;;
    esac
    mise x go@1.26.11 -- go build -buildvcs=true -trimpath \
      -ldflags "-X main.GitCommit=$commit -X main.GitDate=$commit_date -X $version_symbol=$version -X $package.Meta=" \
      -o "$output" "./$component/cmd"
  fi
  mise x go@1.26.11 -- go version -m "$output"
)
```

Check the embedded Go version, target architecture and linker identity. The
qualified op-node build retained build paths, so a rebuild in a different
directory can produce a different hash. Compare each SHA-256 before treating
the bytes as interchangeable. Assemble release images from the frozen qualified
payload.

### OP Deployer from a source ZIP

For v2.9.0, use `component-sources/op-deployer/` and the contract build in
that tree's `ops/docker/op-stack-go/Dockerfile`. Verify the Foundry release
archive against `op-deployer/pkg/deployer/forge/version.json`, put its
`forge` on your PATH, and install the four solc versions from that Dockerfile
(0.8.15, 0.8.19, 0.8.25 and 0.8.28) for your build platform. Start outside a
Git checkout in the directory where you extracted the source ZIP.

```sh
set -eu
arch=arm64 # or amd64
metadata=release-metadata/sources.json
source_root=component-sources/op-deployer
commit=$(jq -er '.inputs.components["op-deployer"].build_commit' "$metadata")
version=$(jq -er '.inputs.components["op-deployer"].version' "$metadata")
commit_date=$(jq -er --arg p "$source_root" --arg c "$commit" \
  '.sources[] | select(.prefix == $p and .commit == $c) | .commit_timestamp' "$metadata")

(
  cd "$source_root"
  export GOTOOLCHAIN=go1.26.11 GOFLAGS="-mod=readonly -p=4" TZ=UTC
  (cd packages/contracts-bedrock && forge build --offline --skip '/**/test/**')
  mise x go@1.26.11 -- go run ./op-deployer/pkg/deployer/artifacts/cmd/mktar \
    -base ./packages/contracts-bedrock \
    -out ./op-deployer/pkg/deployer/artifacts/forge-artifacts/artifacts.tzst \
    --exclude='*.t.sol'
  mise x yq@4.53.6 -- bash op-core/superchain/sync-superchain.sh
  GOOS=linux GOARCH="$arch" TARGETOS=linux TARGETARCH="$arch" CGO_ENABLED=0 \
    GITCOMMIT="$commit" GITDATE="$commit_date" VERSION="$version" \
    mise x just@1.46.0 go@1.26.11 -- just --justfile op-deployer/justfile build-go
  mise x go@1.26.11 -- go version -m op-deployer/bin/op-deployer
)
```

The embedded tarball records filesystem times, and the packer emits a
`COMMIT` entry only when Git metadata is available. Compare the generated
contract artifacts with the reviewed inputs to check the rebuild. Use the
frozen qualified payload for release image assembly.

### Multicannon and Kona-host

For multicannon, use the reviewed platform-specific historical imports
and verify their digests before building the current Cannon embed and wrapper.
Kona-host has a separate pinned Rust/Cargo build recipe.

### Reference Docker targets

For a Git checkout at the selected maintained Optimism source and with
`OP_NODE_VERSION` set to the selected version:

```sh
docker buildx build --platform linux/amd64 --load \
  --file ops/docker/op-stack-go/Dockerfile --target op-node-target \
  --build-arg "OP_NODE_VERSION=${OP_NODE_VERSION:?Set the selected OP component version}" \
  --build-arg "GIT_COMMIT=$(git rev-parse HEAD)" \
  --build-arg "GIT_DATE=$(git show --no-show-signature -s --format=%ct HEAD)" \
  --tag op-node:local .
docker run --rm --entrypoint /usr/local/bin/op-node op-node:local --version
```

The Docker targets below document the upstream recipe entry points. Select
the exact recipe and compiler from the reviewed build materials. A
`GO_VERSION=1.26.11` build argument only works on a recipe that declares it,
such as the maintained fork's compiler-selection change in OP #27. Component
versions follow their individual upstream bases.

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

This is a binary assembly stage, not a complete GitHub release. Complete the
source/examples exports, image publication and qualification before finalizing
the external receipt below.

### Export source and deployment examples

Use reviewed local checkouts containing the exact input commits and all pinned
submodule objects. Additional checkouts can supply objects for older component
and nested submodule revisions. Prepare them using the approved source-access
identity before export; the release repository token alone cannot read the
private maintained OP repository.

```sh
python3 .github/scripts/distribution-sources.py \
  --inputs /path/to/distribution-inputs.json \
  --world-chain-repo /path/to/world-chain \
  --optimism-repo /path/to/optimism \
  --repository /path/to/selected-deployer-checkout \
  --repository /path/to/selected-kona-checkout \
  --build-record /path/to/reviewed-build-record.json \
  --output /path/to/new-source-export
```

The build record uses the same schema and exact inputs as the finalization stage
below. Its reviewed materials identify the corresponding historical Cannon,
Foundry and other imported sources, build recipes and notices. The exporter binds
these records without establishing their authenticity or completeness.

The command reads committed Git objects, preserving dirty and untracked checkout
data. It resolves each gitlink at its pinned commit and includes separately built
components under `component-sources/<component>/`. Missing local objects fail
without fetching. Git replacement objects are disabled. Each blob is checked
against its Git identity; unresolved LFS pointers, escaping symlinks, unsafe paths
and excessive source graphs/data are rejected. Limits are 100000 files, ten GiB
of file contents, 512 MiB per blob and a submodule depth of 16.

The two ZIPs preserve file/executable/symlink modes and deterministic timestamps
from the World Chain source commit. Each contains `release-metadata/sources.json`
with the exact inputs, source trees/gitlinks, build-root commit timestamps and
producer materials, plus a file index containing Git IDs, sizes and SHA-256s.
The examples ZIP includes committed
`examples/distribution/` files and their linked local Markdown guides. Source-file
links use the corresponding source ZIP. Local checkout paths are excluded from
source metadata.

The intermediate `source-manifest.json` records ZIP checksums. Existing output is
preserved, and a failed export retains `INCOMPLETE`/partial output. Finalization
rejects an incomplete source or binary assembly. Independently verify source
coverage, notice obligations, examples and build reproduction before signing or
publishing these assets. This step produces local unsigned assets.

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

Set the target with `--platform`; no manual `TARGETARCH` override is needed.
`runtime-tools` declares that automatic argument, and the distribution targets
inherit it through their parent stages, following [Docker's ARG scoping rules](https://docs.docker.com/build/building/variables/#scoping).

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
The table-based release-page layout is approved under BLO-1252. The existing
release environment approval remains required for publication.

### Finalize the unsigned release assets

After qualifying the payloads and corresponding source/examples ZIPs, record
their exact inputs and resolved build materials. The build record is a JSON
object with exactly `schema_version: 1`, `inputs` (the complete assembly inputs)
and a nonempty `materials` object. Preserve the reviewed compiler/toolchain
versions, lockfile and recipe identities, immutable external image/artifact
digests, historical Cannon sources and imports, Foundry source/version, runtime
libraries and notice/source inventory in `materials`. This field preserves the
producer's records; the finalizer cannot establish their authenticity or coverage.

The image record has exactly `schema_version: 1`, the same `inputs`, and `images`.
`images` maps each of `world-chain`, `op` and `combined` to these fields:

| Field | Required value |
| --- | --- |
| `reference` | The exact planned GHCR variant tag from `distribution-inputs.py` |
| `index_digest` | The published multi-platform image index's `sha256:` digest |
| `platforms` | Exactly `linux/amd64` and `linux/arm64`, each with `manifest_digest` and `config_digest` |

Resolve the tag/index/platform/configuration bindings against the registry and
verify their actual bytes and media types before preparing this record. Do not
copy Docker image-inspect `Id` into a configuration field: it may identify an OCI
index. The finalizer checks record structure, planned tags and distinct variant
indexes; it performs no registry access and cannot prove those bindings itself.

Each source/examples ZIP must contain one `release-metadata/sources.json` with an
`inputs` field equal to the assembly inputs. The finalizer checks this binding
and hashes the complete ZIP bytes. The source producer must independently verify
the committed trees, submodules, component sources, licenses and example contents;
matching metadata alone does not establish corresponding-source completeness.

```sh
python3 .github/scripts/distribution-release.py \
  --assembly /path/to/binary-assembly \
  --sources /path/to/world-chain-v2.9.0-sources.zip \
  --examples /path/to/world-chain-v2.9.0-examples.zip \
  --build-record /path/to/reviewed-build-record.json \
  --image-record /path/to/verified-registry-record.json \
  --output /path/to/new-release-assets
```

The command creates a new private directory, preserving the six archive bytes
and copying the source/examples ZIPs under their planned names. It recomputes
archive hashes and verifies every tar member, mode and payload hash against the
assembly receipt; unexpected, duplicated or missing members fail. The external
`release-manifest.json` binds the eight assets, payload files, inputs, materials,
image descriptors and finalizer script hashes. `SHA256SUMS` covers those eight
assets and the manifest. The manifest excludes its own digest; `SHA256SUMS`
excludes itself. Existing output is never overwritten; failures retain
`INCOMPLETE` and must not be signed/uploaded.

The output is unsigned. Preserve the existing per-archive release signatures;
also sign `SHA256SUMS` and `release-manifest.json` with the established release
GPG identity, and verify the detached signatures with its independently trusted
public key. Before signing, require a zero exit, no `INCOMPLETE`, independent
artifact/runtime qualification, reviewed materials and existing release approval.
Do not substitute an SSH commit-signing key for the release identity. This offline
command does not activate the publishing workflow or authorize publication.
