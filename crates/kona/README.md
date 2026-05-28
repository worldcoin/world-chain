# world-chain-kona

**In-process Kona OP Stack consensus node integration for World Chain.**

This crate integrates the [Kona](https://github.com/anton-rs/kona) OP Stack consensus/derivation
node into World Chain so that both the consensus layer and the Reth execution engine run as a
**single binary**, communicating via direct Rust function calls rather than the Engine API over
HTTP/IPC.

## Motivation

Currently, OP Stack chains run two separate processes:

1. **op-node** (Go) — consensus/derivation layer
2. **reth** (Rust) — execution engine

These communicate over the [Engine API](https://github.com/ethereum/execution-apis/tree/main/src/engine)
via authenticated HTTP (JWT), introducing:

- **Serialization overhead**: Every `engine_newPayload` and `engine_forkchoiceUpdated` call
  requires JSON serialization/deserialization of potentially large payloads.
- **Network latency**: Even on localhost, HTTP adds ~0.1-1ms per round-trip.
- **Operational complexity**: Two processes to deploy, monitor, and restart. Crash recovery
  must handle process-level failures.
- **Resource waste**: Two separate runtimes, memory allocators, and I/O systems.

By embedding Kona (a Rust OP Stack consensus node) directly into the World Chain binary, we
eliminate all of these costs.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    world-chain binary                         │
│                                                               │
│  ┌──────────────────────┐                                     │
│  │   Kona Consensus     │  Rust fn calls                      │
│  │   ┌──────────────┐   │  (no HTTP/IPC)   ┌────────────────┐│
│  │   │ L1 Watcher   │   │                   │  Reth Engine   ││
│  │   │ Derivation   │───│──────────────────►│  (engine tree) ││
│  │   │ P2P Network  │   │                   │  (payload      ││
│  │   │ RPC Server   │   │                   │   builder)     ││
│  │   └──────────────┘   │                   │  (state DB)    ││
│  └──────────────────────┘                   └────────────────┘│
└─────────────────────────────────────────────────────────────┘
```

### Key Components

- **`InProcessEngineClient`** — Implements Kona's `EngineClient` trait (which extends
  `OpEngineApi`) by dispatching to reth's `ConsensusEngineHandle` and reading chain data from
  reth's provider. This is the **zero-overhead bridge** between consensus and execution.

- **`KonaServiceHandle`** — Manages the lifecycle of Kona's actor system (L1 watcher,
  derivation pipeline, engine actor, network actor) within the same tokio runtime as reth.

- **`KonaConfig`** — Bridges World Chain's configuration to Kona's builder requirements.

## Status

This is a **work-in-progress** (WIP) integration. The following items track completion:

### Done ✅

- [x] Crate structure and module layout
- [x] `InProcessEngineClient` struct with all `EngineClient` + `OpEngineApi` method signatures
- [x] `KonaServiceHandle` lifecycle management with cancellation token
- [x] `KonaConfig` for bridging configuration
- [x] Dependency wiring (Cargo.toml with pinned kona revision)
- [x] Comprehensive documentation and architecture description

### Remaining 🚧

- [ ] **Type conversions** (highest priority):
  - `ExecutionPayloadV3` → `OpExecutionData` (reth's internal payload type)
  - `OpPayloadAttributes` (kona/alloy) → reth's `OpPayloadAttributes`
  - `OpBuiltPayload` → `OpExecutionPayloadEnvelopeV3/V4`
  - Reth's `Block` → alloy's `Block<Transaction>`

- [ ] **Provider reads**: Implement `get_l2_block`, `l2_block_by_label`, `get_proof` by reading
  from reth's database provider instead of returning stubs.

- [ ] **Full actor wiring**: Construct Kona's `EngineActor`, `DerivationActor`, `L1WatcherActor`,
  and `NetworkActor` manually with the in-process engine client. Currently `RollupNode::start()`
  constructs its own `OpEngineClient` internally, so we need to either:
  - Upstream a change to Kona to accept an injected engine client, or
  - Wire the actors manually (more work but doesn't require upstream changes)

- [ ] **Dependency alignment**: Resolve the op-alloy version mismatch between kona (0.22.x) and
  world-chain (0.23.x). This will require coordinating with the kona team.

- [ ] **Integration with reth's node builder**: Hook into reth's `NodeHandle` to extract the
  `ConsensusEngineHandle` and `PayloadStore` after launch.

- [ ] **Configuration**: Bridge world-chain's CLI args to `KonaConfig` (L1 RPC, beacon URL, etc.)

- [ ] **Integration tests**: End-to-end test with a devnet.

## Building

Since this crate is excluded from the default workspace build (due to dependency version
conflicts with kona), build it explicitly:

```bash
# Check compilation (best effort — full build requires dep alignment)
cargo check -p world-chain-kona
```

## Dependencies

This crate bridges two major dependency trees:

| Component | Source | Key Versions |
|-----------|--------|--------------|
| Kona | `anton-rs/kona@2586fc56` | alloy 1.1.3, op-alloy 0.22.4 |
| Reth | `worldcoin/reth@4c1b4a1` | alloy 1.1.2, op-alloy 0.23.1 |

The minor version differences in alloy/op-alloy need to be resolved for a clean build.

## Related Work

- [Kona Node Architecture](https://github.com/anton-rs/kona/tree/main/crates/node) — Kona's
  actor-based consensus node
- [Reth Engine API](https://github.com/paradigmxyz/reth/tree/main/crates/rpc/rpc-engine-api) —
  Reth's Engine API handler
- [OP Stack Engine API Spec](https://specs.optimism.io/protocol/exec-engine.html) — The protocol
  that this integration replaces with in-process calls
