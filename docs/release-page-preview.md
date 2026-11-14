> **Layout preview:** version numbers and asset links below are illustrative.
> This is not a published release or qualified deployment guide.

# World Chain v2.8.3

## Changes

- Adds World Chain-only, OP-only and combined distribution choices.
- Includes Docker Compose, systemd and standard configuration examples.

**Upgrade notes:** Review the release's configuration and database compatibility
notes before upgrading an existing node.

## Deploy with the combined image (recommended)

The execution and rollup services run in separate containers using the same image.
All executables are in `/usr/local/bin`.

1. **Download and extract the [deployment examples]({{EXAMPLES_ZIP_URL}}).**

    ```sh
    unzip world-chain-v2.8.3-examples.zip -d world-chain-release
    cd world-chain-release/examples/distribution
    ```

2. **Choose the network and copy its configuration.** For mainnet:

    ```sh
    cp -n node.mainnet.env.example node.env
    ```

    For Sepolia, use `node.env.example` instead.

3. **Edit `node.env`.** Set `COMBINED_IMAGE` to the combined image digest from the
   [release manifest]({{MANIFEST_URL}}), supply the L1 execution and beacon RPC
   endpoints, and set the data-directory UID/GID.

4. **Prepare private storage and the shared Engine JWT.** For a new installation:

    ```sh
    chmod 600 node.env
    install -d -m 0700 data/execution data/op-node
    (umask 077; set -C; openssl rand -hex 32 > jwt.hex)
    ```

5. **Start the services and check their status.**

    ```sh
    docker compose --env-file node.env -f compose.combined.yaml up -d
    docker compose --env-file node.env -f compose.combined.yaml ps
    ```

   Follow the [sync and readiness checks]({{DEPLOYMENT_GUIDE_URL}}) before serving
   RPC traffic. The standard guide also describes qualified builder configuration.

## Deploy with individual images

| Image | Contents | Default executable |
| --- | --- | --- |
| `ghcr.io/worldcoin/world-chain:v2.8.3` | World Chain and existing runtime tools | `world-chain` |
| `ghcr.io/worldcoin/world-chain:op-v2.8.3` | All OP binaries, operator tools and helpers | `op-node` |

1. **Prepare `node.env`, storage and JWT** using the standard setup above.
2. **Set `WORLD_CHAIN_IMAGE` and `OP_IMAGE`** to their respective release digests.
3. **Start the services with the individual-image Compose file.**

    ```sh
    docker compose --env-file node.env -f compose.individual.yaml up -d
    docker compose --env-file node.env -f compose.individual.yaml ps
    ```

## Deploy the binaries with systemd

| Bundle | amd64 | arm64 |
| --- | --- | --- |
| Combined (recommended) | [tar.gz]({{COMBINED_AMD64_URL}}) | [tar.gz]({{COMBINED_ARM64_URL}}) |
| World Chain | [tar.gz]({{WORLD_CHAIN_AMD64_URL}}) | [tar.gz]({{WORLD_CHAIN_ARM64_URL}}) |
| OP binaries and helpers | [tar.gz]({{OP_AMD64_URL}}) | [tar.gz]({{OP_ARM64_URL}}) |

1. **Download the appropriate bundle**, [checksums]({{CHECKSUMS_URL}}) and
   [release signatures]({{SIGNATURES_URL}}).
2. **Verify the signature and checksum, then extract the archive.** The
   [binary installation guide]({{SYSTEMD_GUIDE_URL}}) supplies the verification
   commands and supported native library requirements.
3. **Install the executables in `/usr/local/bin`** using the guide's installation
   command. Keep the included licenses and notices.
4. **Install standard configuration and service units** as described in the guide:
   `/etc/world-chain/node.env`, the Engine JWT, `world-chain.service` and
   `op-node.service`.
5. **Enable the services and check their status.**

    ```sh
    sudo systemctl enable --now world-chain.service op-node.service
    sudo systemctl status world-chain.service op-node.service
    ```

## Sources and build instructions

| Resource | Download or guide |
| --- | --- |
| World Chain and maintained OP sources | [Source ZIP]({{SOURCES_ZIP_URL}}) |
| Docker build instructions | [Build guide]({{BUILD_GUIDE_URL}}) |
| Compose, systemd and standard configuration | [Examples ZIP]({{EXAMPLES_ZIP_URL}}) |
| Artifact verification | [Checksums]({{CHECKSUMS_URL}}) · [Signatures]({{SIGNATURES_URL}}) |

<details>
<summary>Component versions, image digests and build provenance</summary>

The release/image variant version is v2.8.3. OP versions retain their own
upstream-based Worldchain suffix; Foundry tools retain their Foundry versions.

| Component | Shipped version | Upstream base |
| --- | --- | --- |
| World Chain | v2.8.3 | {{RETH_BASE}} |
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
