# Deployment examples

These examples define the standard follower configuration for the World Chain,
OP and combined distributions. Select image digests from the release manifest.
Verify the selected artifacts and deployment before serving traffic. Review the
[upgrade and rollback guide](../../docs/upgrading-v2.9.0.md) before reusing data
or pairing the execution client with an operator-managed op-node.

Run the commands from `examples/distribution/`. The examples ZIP retains the
repository-relative `examples/` and `docs/` layout so the included guide links work.

They target Linux hosts with RPC/follower configurations for Sepolia
(`node.sepolia.env.example`) and mainnet (`node.mainnet.env.example`). The same
configuration paths are used with Compose and systemd. Choose one network per
deployment directory/host setup and use its matching L1 endpoints; do not reuse
one database for different networks or concurrent processes.
The mainnet selectors differ: execution uses `--chain worldchain`, while op-node
uses `--network=worldchain-mainnet`. Both use `worldchain-sepolia` on Sepolia.
Set the supported sequencer forwarding URL in the execution client's
`--rollup.sequencer-http` option if the RPC should submit transactions.

## 1. Standard deployment: combined image

The two containers select different executables from the same image. Linux host
networking lets both processes use the same loopback Engine API as the systemd
deployment. RPC and Engine bind to loopback; P2P uses the clients' P2P ports. Do
not run Compose and systemd against the same ports or data directories together.

```sh
cp -n node.sepolia.env.example node.env
chmod 600 node.env
install -d -m 0700 data/execution data/op-node
(umask 077; set -C; openssl rand -hex 32 > jwt.hex)
```

For mainnet, copy `node.mainnet.env.example` instead of `node.sepolia.env.example`.

Edit `node.env`: set qualified image digests, your data-directory UID/GID, L1
execution and beacon endpoints, and review the chain/flags. The generated JWT is
a local Engine API credential shared by these two services, not a blockchain key.
Keep the file on restart; do not regenerate it for a running deployment. Keep
the state directories private: the current local-identity peerstore can contain
a copy of the node's private P2P key.

```sh
docker compose --env-file node.env -f compose.combined.yaml config --quiet
docker compose --env-file node.env -f compose.combined.yaml up -d
docker compose --env-file node.env -f compose.combined.yaml logs --tail=100
```

Use an authenticated reverse proxy if exposing the RPC beyond the host. Engine
port 8551 and op-node administration must remain private. Container startup and
process ordering do not prove readiness: check chain ID, sync/head progress, L1
derivation and peers before serving RPC traffic.

## 2. Individual images

Use the same `node.env`, JWT and storage setup. Set `WORLD_CHAIN_IMAGE` to the
World Chain-only digest and `OP_IMAGE` to the OP-only digest:

```sh
docker compose --env-file node.env -f compose.individual.yaml config --quiet
docker compose --env-file node.env -f compose.individual.yaml up -d
```

Choose one deployment mode. To switch, stop the old services cleanly first and
reuse compatible data/configuration with the selected release's upgrade notes.
Backups and database format compatibility determine rollback safety.

## 3. Direct binaries and systemd

Download the appropriate architecture bundle, `SHA256SUMS` and release signatures.
Verify the signature using the established release signing identity, then verify
the downloaded archive against `SHA256SUMS`. Inspect/extract the archive into a
staging directory; install its executable payload into `/usr/local/bin` and retain
licenses, notices and runtime-library instructions. Do not install a binary for a
different architecture or assume GNU binaries are statically linked.

For a combined bundle extracted into `./payload`, install the listed executables:

```sh
sudo install -m 0755 payload/world-chain payload/op-node payload/op-batcher \
  payload/op-proposer payload/op-conductor payload/op-challenger \
  payload/op-deployer payload/op-dispute-mon payload/cannon payload/kona-host \
  payload/forge payload/cast /usr/local/bin/
```

Create a dedicated `worldchain` system user/group using your host's provisioning
mechanism. For a new installation, prepare configuration and a new Engine JWT:

```sh
sudo install -d -o root -g worldchain -m 0750 /etc/world-chain
sudo install -o root -g root -m 0600 node.env /etc/world-chain/node.env
sudo install -o root -g worldchain -m 0640 jwt.hex /etc/world-chain/jwt.hex
sudo install -m 0644 systemd/world-chain.service systemd/op-node.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now world-chain.service op-node.service
sudo systemctl status world-chain.service op-node.service
sudo journalctl -u world-chain.service -u op-node.service --since '10 minutes ago'
```

The units use `EnvironmentFile` and systemd's argument expansion, not a shell:
`WORLD_CHAIN_ARGS` and `OP_NODE_ARGS` contain ordinary CLI arguments. Endpoint
credentials stay in the root-readable environment file. State directories are
created under `/var/lib/world-chain`; orderly shutdown gets up to three minutes.
After repeated failures the start limit requires operator intervention.

To stop before an upgrade, use `sudo systemctl stop op-node world-chain`. Follow
the release's database compatibility/backup guidance before replacing binaries,
then start the services and qualify sync/readiness again.

## Builder configuration

The image and installation layout also serve builders, but this follower example
does not enable sequencing or builder signing. A production builder needs its
authorized signer integration, PBH/Flashblocks configuration and coordinator
connections. Enabling `--builder.enabled` requires an explicit
`--builder.private-key` or `--builder.signer-config`; there is no default key.
Protected signing uses `--builder.signer-config` for PBH and
`--flashblocks.signer-config` for publication. An in-process grant signer also
uses `--flashblocks.authorizer-signer-config` and an independently pinned
`--flashblocks.authorizer-vk`. Follow the
[signer integration guide](../../docs/signer-integration.md) for the mTLS
configuration and role requirements.

See [Docker build instructions](../../docs/distribution-build.md).
