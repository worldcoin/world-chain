> **Format preview — awaiting approval.** Versions, links and digests below are
> placeholders. This is not a published or qualified release.

# World Chain {{VERSION}}

World Chain for builders and RPC operators, with maintained OP components and
operator tooling. Supports Linux amd64 and arm64.

## Standard deployment: combined image

**Recommended:** run the execution and rollup services in separate containers
using the same combined image. All executables are in `/usr/local/bin`.

```text
ghcr.io/worldcoin/world-chain:{{VERSION}}-combined
Pinned image: ghcr.io/worldcoin/world-chain@sha256:{{COMBINED_DIGEST}}
```

Extract **[deployment examples]({{EXAMPLES_ZIP_URL}})** and enter
`examples/distribution/`. Edit `node.env` with the
release digest, network and L1 endpoints, and follow the included JWT/storage setup:

```sh
docker compose --env-file node.env -f compose.combined.yaml config --quiet
docker compose --env-file node.env -f compose.combined.yaml up -d
```

See **[standard configuration and builder/RPC setup]({{DEPLOYMENT_GUIDE_URL}})**.
This release must state which builder signing configuration is qualified.

## Individual images

| Image | Contents | Default process |
| --- | --- | --- |
| `ghcr.io/worldcoin/world-chain:{{VERSION}}` | World Chain and existing runtime tooling | `world-chain` |
| `ghcr.io/worldcoin/world-chain:{{VERSION}}-op` | Maintained OP services, operator tools and required helpers | `op-node` |

Use the same standard configuration with `compose.individual.yaml`. Pin the
digests from [release-manifest.json]({{MANIFEST_URL}}), rather than mutable tags.

```sh
docker compose --env-file node.env -f compose.individual.yaml up -d
```

## Binary deployment: systemd

Download the **combined**, **World Chain-only**, or **OP-only** binary bundle for
your architecture. Verify release signatures and checksums, install the payload
in `/usr/local/bin`, and follow **[direct binary/systemd instructions]({{SYSTEMD_GUIDE_URL}})**.
The examples ZIP includes standard configuration and `world-chain.service` /
`op-node.service`; the guide states native runtime-library requirements.

```sh
sudo systemctl enable --now world-chain.service op-node.service
sudo systemctl status world-chain.service op-node.service
```

## Changes and upgrade notes

- **Highlights:** {{HIGHLIGHTS}}
- **Required configuration changes:** {{CONFIG_CHANGES}}
- **Network/hardfork compatibility:** {{NETWORK_COMPATIBILITY}}
- **Database compatibility, backup and rollback:** {{DATABASE_NOTES}}
- **Known issues and limitations:** {{KNOWN_ISSUES}}

## Component versions

The release/image variant version is {{VERSION}}. OP versions retain their own
upstream-based Worldchain suffix; Foundry tools retain their Foundry versions.

| Component | Shipped version | Upstream base |
| --- | --- | --- |
| World Chain | {{VERSION}} | {{RETH_BASE}} |
| op-node | {{OP_NODE_VERSION}} | {{OP_NODE_BASE}} |
| op-batcher | {{OP_BATCHER_VERSION}} | {{OP_BATCHER_BASE}} |
| op-proposer | {{OP_PROPOSER_VERSION}} | {{OP_PROPOSER_BASE}} |
| op-conductor | {{OP_CONDUCTOR_VERSION}} | {{OP_CONDUCTOR_BASE}} |
| op-challenger | {{OP_CHALLENGER_VERSION}} | {{OP_CHALLENGER_BASE}} |
| op-deployer | {{OP_DEPLOYER_VERSION}} | {{OP_DEPLOYER_BASE}} |
| op-dispute-mon | {{OP_DISPUTE_MON_VERSION}} | {{OP_DISPUTE_MON_BASE}} |
| cannon / kona-host | {{HELPER_VERSIONS}} | {{HELPER_BASES}} |
| forge / cast | {{FOUNDRY_VERSIONS}} | Foundry |

## Downloads and verification

| Download | amd64 | arm64 |
| --- | --- | --- |
| Combined binaries (standard) | [tar.gz]({{COMBINED_AMD64_URL}}) | [tar.gz]({{COMBINED_ARM64_URL}}) |
| World Chain binaries | [tar.gz]({{WORLD_CHAIN_AMD64_URL}}) | [tar.gz]({{WORLD_CHAIN_ARM64_URL}}) |
| OP binaries and helpers | [tar.gz]({{OP_AMD64_URL}}) | [tar.gz]({{OP_ARM64_URL}}) |

**[Source ZIP: World Chain + maintained Optimism]({{SOURCES_ZIP_URL}})** ·
**[Compose, systemd and configuration examples]({{EXAMPLES_ZIP_URL}})** ·
**[SHA256SUMS]({{CHECKSUMS_URL}})** · **[signatures]({{SIGNATURES_URL}})** ·
**[release manifest]({{MANIFEST_URL}})**

## Build from source

Use the source ZIP and **[Docker build instructions]({{BUILD_GUIDE_URL}})** for
each image variant. Source commits, component bases, resolved build materials,
payload checksums, platform image digests and qualification evidence are recorded
in the release manifest.
