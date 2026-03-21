# Development Guide

## Project Structure

```
world-chain/
├── crates/
│   ├── bin/world-chain/             # Node binary
│   ├── world-chain-primitives/      # Core types (flashblock payloads, BAL, P2P auth)
│   ├── world-chain-cli/             # CLI args & node configuration
│   ├── world-chain-pbh/             # Priority Bundle Handler (nullifiers, proofs)
│   ├── world-chain-p2p/             # Flashblocks P2P sub-protocol
│   ├── world-chain-pool/            # Transaction pool with PBH ordering
│   ├── world-chain-rpc/             # JSON-RPC APIs (ETH, OP, Engine, Sequencer)
│   ├── world-chain-builder/         # Block builder, BAL, parallel validation
│   ├── world-chain-payload/         # Payload generation & job management
│   ├── world-chain-engine/          # Pending Block Consumer, and Engine Tree management.
│   ├── world-chain-node/            # Node builder components.
│   └── world-chain-test-utils/      # Shared test utilities & e2e harness
├── e2e-tests/                       # Integration & property-based tests
├── xtask/                           # Development tooling
├── contracts/                       # Solidity smart contracts
├── specs/                           # Protocol Specifications
└── scripts/                         # Extra scripts
```

### Local Builder Playground

Spawn an in-process node swarm — no Docker:

```bash
cargo xtask launch-node --nodes 2 --spam --flashblocks
```

Starts N nodes via P2P, drives block production with `EngineDriver`, optionally runs `TxSpammer`. Prints RPC URLs for `cast` interaction.

## Metrics

Custom metrics are exposed via the standard reth metrics endpoint (`/metrics`). Key namespaces:

### Builder (`world-chain-builder`)
- `flashblocks_per_epoch` — Histogram of flashblocks produced per epoch
- `coordinator.*` — Execution coordinator timing and throughput

### Payload (`world-chain-payload`)
- `payload_build_time` — Time to build a payload
- `payload_job.*` — Job lifecycle metrics

### P2P (`world-chain-p2p`)
- `flashblocks_received` — Counter of flashblocks received from peers
- `flashblocks_sent` — Counter of flashblocks propagated
- `peer_latency` — Per-peer latency scoring for receive-peer rotation

### Engine (`world-chain-engine`)
- `validation_time` — Time to validate a flashblock
- `state_root_compute_time` — Parallel state root computation time

## World Chain Snapshots

`reth` snapshots are regularly updated:

```bash
BUCKET="world-chain-snapshots" # use world-chain-testnet-snapshots for sepolia
FILE_NAME="reth_archive.tar.lz4" # reth_full.tar.lz4 is available on mainnet only
OUT_DIR="./"
VID="$(aws s3api head-object --bucket "$BUCKET" --key "$FILE_NAME" --region eu-central-2 --query 'VersionId' --output text)"
aws s3api get-object --bucket "$BUCKET" --key "$FILE_NAME" --version-id "$VID" --region eu-central-2 --no-cli-pager /dev/stdout | lz4 -d | tar -C "$OUT_DIR" -x
```
