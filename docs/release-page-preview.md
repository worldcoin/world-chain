> **Layout preview:** version numbers and asset links are illustrative.

# World Chain v2.8.3

## Changes

- Adds World Chain-only, OP-only and combined distribution choices.
- Includes Docker Compose, systemd and standard configuration examples.

**Upgrade notes:** Review the release's configuration and database compatibility
notes before upgrading an existing node.

## Combined image (recommended)

World Chain and all OP binaries in one image for builders and RPC operators.
Run each selected service in its own container.

| Image | Platforms | Instructions | Verification |
| --- | --- | --- | --- |
| [combined-v2.8.3]({{COMBINED_IMAGE_URL}}) | Linux amd64 / arm64 | [Docker Compose setup]({{DEPLOYMENT_GUIDE_URL}}) | [Image digests]({{MANIFEST_URL}}) |

## Individual images

Separate World Chain and OP-only images, using the same standard configuration.
The OP image includes all OP services, operator tools and required helpers.

| Image | Platforms | Instructions | Verification |
| --- | --- | --- | --- |
| [World Chain: v2.8.3]({{WORLD_CHAIN_IMAGE_URL}}) | Linux amd64 / arm64 | [World Chain image setup]({{WORLD_CHAIN_IMAGE_GUIDE_URL}}) | [Image digests]({{MANIFEST_URL}}) |
| [OP bundle: op-v2.8.3]({{OP_IMAGE_URL}}) | Linux amd64 / arm64 | [OP image setup]({{OP_IMAGE_GUIDE_URL}}) | [Image digests]({{MANIFEST_URL}}) |

## Binary bundles

Install the executables directly in `/usr/local/bin`, with standard configuration
and systemd units supplied in the deployment examples.

| Bundle | Linux amd64 | Linux arm64 | Instructions | Verification |
| --- | --- | --- | --- | --- |
| Combined | [tar.gz]({{COMBINED_AMD64_URL}}) | [tar.gz]({{COMBINED_ARM64_URL}}) | [Combined binaries and systemd]({{COMBINED_BINARY_GUIDE_URL}}) | [Checksums]({{CHECKSUMS_URL}}) |
| World Chain | [tar.gz]({{WORLD_CHAIN_AMD64_URL}}) | [tar.gz]({{WORLD_CHAIN_ARM64_URL}}) | [World Chain binary setup]({{WORLD_CHAIN_BINARY_GUIDE_URL}}) | [Checksums]({{CHECKSUMS_URL}}) |
| OP binaries and helpers | [tar.gz]({{OP_AMD64_URL}}) | [tar.gz]({{OP_ARM64_URL}}) | [OP binary setup]({{OP_BINARY_GUIDE_URL}}) | [Checksums]({{CHECKSUMS_URL}}) |

## Source and examples

Build from the pinned sources or use the supplied Compose, systemd and network
configuration examples.

| Resource | Download | Instructions | Verification |
| --- | --- | --- | --- |
| World Chain and maintained OP sources | [Source ZIP]({{SOURCES_ZIP_URL}}) | [Docker build instructions]({{BUILD_GUIDE_URL}}) | [Checksums]({{CHECKSUMS_URL}}) |
| Compose, systemd and configuration | [Examples ZIP]({{EXAMPLES_ZIP_URL}}) | [Deployment guide]({{DEPLOYMENT_GUIDE_URL}}) | [Checksums]({{CHECKSUMS_URL}}) |

## Checksums and provenance

Verify downloaded files and inspect the shipped component versions, source
commits and image digests.

| Resource | Link | Instructions |
| --- | --- | --- |
| SHA-256 checksums | [SHA256SUMS]({{CHECKSUMS_URL}}) | [Checksum verification]({{VERIFICATION_GUIDE_URL}}) |
| Release signatures | [Signatures]({{SIGNATURES_URL}}) | [Signature verification]({{SIGNATURE_GUIDE_URL}}) |
| Component versions and build provenance | [Release manifest]({{MANIFEST_URL}}) | [Manifest and image digest verification]({{MANIFEST_GUIDE_URL}}) |
