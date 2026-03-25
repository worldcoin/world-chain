# Development Guide

## Project Structure

```
world-chain/
├── crates/
├── bin/world-chain/                     # Node binary
│   ├── primitives/                   # Core types (flashblock payloads, BAL, P2P auth)
│   ├── cli/                          # CLI args & node configuration
│   ├── pbh/                          # Priority Bundle Handler (nullifiers, proofs)
│   ├── p2p/                          # Flashblocks P2P sub-protocol
│   ├── pool/                         # Transaction pool with PBH ordering
│   ├── rpc/                          # JSON-RPC APIs (ETH, OP, Engine, Sequencer)
│   ├── builder/                      # Block builder, BAL, parallel validation
│   ├── payload/                      # Payload generation & job management
│   ├── node/                         # Node builder components.
│   └── test-utils/                   # Shared test utilities & e2e harness
├── e2e-tests/                       # Integration & property-based tests
├── xtask/                           # Development tooling
├── pkg/contracts/                   # Solidity smart contracts
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
