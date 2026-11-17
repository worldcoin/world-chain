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

See [Preconfirmed log API](preconfirmed-logs.md) for the optional resumable HTTP
and WebSocket log feed, its operating limits, and client recovery requirements.

See [Flashblock signing evidence](flashblock-evidence.md) for optional signed
envelope lookups and independent origin verification.

### Pending state RPCs

With Flashblocks enabled, standard `pending` state reads use the latest executed
Flashblock over its parent state. Calls, gas estimates, access lists, and
`eth_simulateV1` retain one snapshot per request across asynchronous work.
Internal lookups carry the selected block hash; unavailable views fail instead
of switching to newer state. Separate requests can observe different snapshots.
Canonical block tags, explicit hash requests, and `requireCanonical` retain
their existing selector semantics. Canonicalization does not turn an in-flight
pending request into a canonical-hash request. Preconfirmation does not establish
finality.

When no Flashblock is available, state and execution RPCs use the existing
OP-Reth fallback. Errors loading a selected Flashblock propagate; a view that no
longer extends the local canonical tip returns a pending-block-not-found error
and can be retried. Pending block, transaction-by-block, and block-receipt reads
apply the same freshness check before using the selected Flashblock. Pending
transaction counts retain Reth's consecutive transaction-pool nonce adjustment
and can exceed the executed Flashblock nonce.

Receipt misses by transaction hash return `null` even while the cached Flashblock
is stale; provider errors still propagate.

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
