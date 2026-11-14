> **Revised format preview — awaiting approval.** Links, versions and digests
> are placeholders, not a published release.

# World Chain {{VERSION}}

## Changes

- {{CHANGE_1}}
- {{CHANGE_2}}

**Upgrade notes:** {{UPGRADE_NOTES}}

## Deploy

### Combined image (recommended)

Use `ghcr.io/worldcoin/world-chain:combined-{{VERSION}}` for the execution and
rollup containers. Follow the [standard configuration and Compose setup]({{DEPLOYMENT_GUIDE_URL}}), then:

```sh
docker compose --env-file node.env -f compose.combined.yaml up -d
```

### Individual images

- **World Chain:** `ghcr.io/worldcoin/world-chain:{{VERSION}}`
- **OP binaries:** `ghcr.io/worldcoin/world-chain:op-{{VERSION}}`

Use the [individual-image Compose setup]({{INDIVIDUAL_IMAGE_GUIDE_URL}}) with the
same standard configuration.

### Binaries and systemd

Download the combined bundle for [amd64]({{COMBINED_AMD64_URL}}) or
[arm64]({{COMBINED_ARM64_URL}}), or select an individual bundle from the release
assets. Follow the [binary installation and systemd guide]({{SYSTEMD_GUIDE_URL}}).

[Compose, systemd and configuration examples]({{EXAMPLES_ZIP_URL}}) ·
[Docker build instructions]({{BUILD_GUIDE_URL}})

## Sources and verification

[Source ZIP]({{SOURCES_ZIP_URL}}) · [Checksums]({{CHECKSUMS_URL}}) ·
[Signatures]({{SIGNATURES_URL}})

<details>
<summary>Component versions, image digests and build provenance</summary>

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

[Release manifest: source commits, image digests and payload checksums]({{MANIFEST_URL}}).
Pin the appropriate image digest from this manifest when deploying.

</details>
