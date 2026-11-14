# World Chain distributions

World Chain owns the coordinated images and GitHub release. Image variants and
binary bundles use the World Chain release version. Each OP component keeps its own `vX.Y.Z-worldchain[.N]` version,
where `X.Y.Z` is its selected upstream component version. A bundle version does
not assign a common upstream version to all OP components.

The distributions serve World Chain builders and RPC operators. Runtime
configuration selects the node's role. The standard deployment uses the combined
image in multiple containers; separate World Chain and OP images remain available.

| Variant | Proposed version tag | Default executable |
| --- | --- | --- |
| Combined (standard) | `ghcr.io/worldcoin/world-chain:vX.Y.Z-combined` | `world-chain` |
| World Chain (existing image) | `ghcr.io/worldcoin/world-chain:vX.Y.Z` | `world-chain` |
| OP binaries from the maintained fork | `ghcr.io/worldcoin/world-chain:vX.Y.Z-op` | `op-node` |

These are distinct images/manifests, not aliases for the combined payload. Keep
the existing World Chain image's purpose and tag stable. The OP image contains
the OP components and their helpers; the combined image adds World Chain. Each
variant's binary archive uses the same payload as that variant's image.

Install every listed executable in `/usr/local/bin` in the combined image,
including the OP services, operator tools and helpers. Keep those paths stable
for deployment commands and helper configuration.

Initially each container selects one process from the shared image. Preserve the
existing default `/usr/local/bin/world-chain` entrypoint; deployments running an
OP service override the entrypoint/command. A process supervisor is outside the
initial implementation; multiple processes in one container remain a future idea.

This document and `.github/scripts/distribution-inputs.py` define the input and asset
contract. The script validates offline identifiers and emits an asset plan. It
does not build a bundle, verify Git provenance, or change the release workflow.

## Included executables and runtime

The initial node-stack bundle contains:

| Executable | Purpose |
| --- | --- |
| `world-chain` | World Chain execution client |
| `op-node` | OP consensus client |
| `op-batcher` | Batch submission |
| `op-proposer` | Output proposal |
| `op-conductor` | Sequencer coordination |
| `op-challenger` | Fault dispute participation |
| `op-deployer` | OP contract deployment tooling |
| `op-dispute-mon` | Dispute monitoring |
| `cannon` | Challenger VM helper, including supported historical VM embeds |
| `kona-host` | Challenger proof-program host |
| `forge` | Pinned Foundry helper required by `op-deployer` |
| `cast` | Existing World Chain runtime tooling |

Helpers are part of the distribution, rather than something an operator must
download separately. The inherited [challenger image recipe](https://github.com/worldcoin/optimism/blob/0d0588be2aaa7b22a7514649b61928189fb84534/ops/docker/op-stack-go/Dockerfile)
copies `cannon` and `kona-host`, builds historical Cannon implementations into
multicannon, supplies a musl loader/libc, and installs glibc, libgcc, libstdc++,
OpenSSL and CA certificates. Preserve the required payload when assembling the
combined image; copying only `op-challenger` is insufficient. Keep the helper
paths configured through `OP_CHALLENGER_CANNON_BIN`,
`OP_CHALLENGER_ASTERISC_KONA_SERVER` and `OP_CHALLENGER_CANNON_KONA_SERVER`.

The final runtime library set must come from inspecting the actual ELF payloads
on both architectures, including executables extracted by multicannon. Include
their interpreters and transitive shared libraries in the image. Go binaries
include their Go runtime; the runtime image does not need a Go compiler. Shells
used by existing deployment commands must also remain available.

Include the Forge version and per-platform checksums pinned in the selected
Optimism tree's `op-deployer/pkg/deployer/forge/version.json`. The deployer checks
the local Forge version and downloads another copy if it does not match; the
combined image must satisfy that requirement locally. Preserve the deployer's
embedded contract artifacts and their recorded build inputs. Record `cast`'s
version and checksum too; do not silently replace the existing World Chain helper
with an incompatible version. These tools retain their Foundry versions rather
than receiving an OP `-worldchain` suffix.

World Chain currently pins `cast` to Foundry 1.8.6 for its certificate-trust
behavior, while this fork's deployer pins Forge separately. Preserve the World
Chain trust requirements and qualify Forge against them; compatibility with an
older deployer pin is not permission to weaken that baseline.

Binary archives include the helper executables and supported Cannon embeds too.
They must describe the supported Linux hosts and required native library and
interpreter versions; an archive of executables alone is not a statically linked,
portable runtime. Include applicable licenses and notices. Test all shipped
services and helper execution paths on each supported platform before publishing.

## Input contract

The validator accepts a UTF-8 JSON object, limited to 64 KiB, with these fields:

| Field | Required value |
| --- | --- |
| `schema_version` | Integer `1` |
| `bundle_version` | World Chain release version, including leading `v` |
| `sources` | Exactly `world_chain` and `optimism`, each a nonzero full lowercase commit ID |
| `components` | Exactly the nine OP components/helpers in the table above |
| `platforms` | `linux/amd64` and `linux/arm64`, each exactly once |

Each component entry contains exactly `version`, `upstream_tag` and
`upstream_commit`. For example, `op-node` can have version
`v1.19.11-worldchain.2`, upstream tag `op-node/v1.19.11`, and the full resolved
upstream commit. This illustrates the format, not an adopted release baseline.
`cannon` and `kona-host` also need their own reviewed upstream identities; do not
assign them the op-node version. Unknown fields, duplicate JSON fields, floating
source refs, mismatched upstream versions and noncanonical revisions are errors.

```sh
python3 .github/scripts/distribution-inputs.py /path/to/distribution-inputs.json
python3 -m unittest discover -s .github/scripts -p 'test_distribution_inputs.py'
```

The two source commits bind the World Chain build recipes and the maintained
Optimism fork recipes. Inputs are caller-supplied. Before building, the eventual
publisher must resolve and verify every claimed source/tag binding, compare the
bundle version to the selected World Chain `Cargo.toml`, and qualify the common
fork commit against each upstream component base. An ancestor tag alone is not
proof of that base. Use clean exported source trees and committed lockfiles.

The build must also resolve and record external materials, including base-image
digests and historical Cannon imports. Their inherited tag-only references must
not be mistaken for immutable inputs. Record toolchain versions, build recipes,
target platforms and resulting checksums in the release manifest. Syntax checks
and matching commit IDs alone do not establish reproducible builds.

## Release asset contract

For a bundle version such as `v2.8.3`, the plan names:

- `world-chain-combined-v2.8.3-<target>.tar.gz` (standard bundle)
- `world-chain-v2.8.3-<target>.tar.gz` (World Chain; existing naming)
- `world-chain-op-v2.8.3-<target>.tar.gz` (OP binaries and helpers)
- `world-chain-v2.8.3-sources.zip`
- `world-chain-v2.8.3-examples.zip`
- `release-manifest.json`
- `SHA256SUMS`

`<target>` is `x86_64-unknown-linux-gnu` or `aarch64-unknown-linux-gnu`.

Keep `world-chain` at the archive root for existing consumers. Include the other
executables alongside it, plus runtime instructions, licenses and notices. Image
and binary archives must consume the same built payload for each platform, rather
than independently rebuilding it. The manifest must bind each payload file and
archive checksum, source identities, component versions, resolved build materials
and the published image digest. Preserve existing release signatures.

Finalize `release-manifest.json` as an external receipt after producing the
artifacts. Do not embed a receipt containing an image/archive's own digest in
that hashed payload; embed the source/version inputs separately if needed.

The explicit source archive contains both `world-chain/` and `optimism/` trees at
the recorded commits, their pinned submodule contents, and required license and
build metadata. Include or identify the corresponding sources for imported
historical Cannon implementations and other redistributed artifacts. GitHub's
automatic World Chain source archive does not include the other repository or
all submodule contents and cannot replace this asset.

## Deployment documentation and release-page approval

Present release instructions in this order: **combined image**, **individual
images**, then **direct binaries/systemd**. Include Docker Compose examples,
standard configuration, systemd units, Docker build instructions, and the source
ZIP and checksum links. Use the same configuration conventions across deployment
modes; keep network and role configuration explicit.

The [release-page preview](release-page-preview.md) is a draft for user approval.
Do not activate a release-page renderer or publish this format until the user
approves it. The existing release environment approval remains required too.
See [deployment examples](../examples/distribution/README.md) and
[Docker build instructions](distribution-build.md). The examples archive must
include those guides and corresponding Compose, configuration and systemd files.

Reuse the release environment approval and architecture matrix in
`.github/workflows/release.yml`. Private cross-repository checkout needs an
approved scoped identity; the World Chain repository token does not grant access
to another private repository. Do not introduce a personal-token workaround.

Signer-enclave and proof measurements, proof activation, publishing identities,
deployment references and rollout acceptance remain separate reviewed changes.
Including `kona-host` does not authorize changing measured proof programs or
dropping support for games using older measurements.
