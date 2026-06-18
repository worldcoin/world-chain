# Live Pre-image Witness Oracle — Design

Linear: [PROTO-4718 — witness data oracle: design](https://linear.app/worldcoin/issue/PROTO-4718/witness-data-oracle-design)

## Summary

The World Chain proof system builds a range witness (`WorldRangeWitnessData` =
`PreimageStore` + `BlobData` + schedule) by driving a Kona single-chain host against live
L1/L2 RPC. This is the dominant latency in proof generation. This document specifies a
**live pre-image witness oracle**: an opt-in mode in which the World Chain L2 node captures
each block's execution witness *during normal block import* (zero re-execution), caches it in
a bounded in-memory buffer, and serves an entire block range in a single RPC call. The Kona
host pre-seeds its key/value store from that response, so only the small, bounded L1
derivation data still hits the network.

## Motivation: why the current path is slow

`build_range_input` (`proofs/kona-host/src/online.rs:244`) spins up a Kona `SingleChainHost`
(`online.rs:288`) pointed at live RPC endpoints and runs the derivation/execution pipeline,
wrapping the oracle in a `PreimageWitnessCollector`
(`proofs/kona-host/src/witness_generation/preimage_witness_collector.rs:12`) that records every
preimage the guest pulls into a `PreimageStore`
(`proofs/core/src/witness/preimage_store.rs:15`).

For a range of `N` L2 blocks the Kona host hint handler issues, **sequentially**:

- `debug_executePayload` once per block — **re-executes the block** on the node and returns the
  full execution witness (state trie nodes + bytecode + keys). This is the heavy call and the
  dominant cost.
- `debug_getRawHeader` for each referenced L2 header.
- `eth_getProof` for the `L2ToL1MessagePasser` account/storage proofs (boundary output roots)
  and any account/storage proofs not covered by the execution witness.
- `debug_dbGet` fallbacks when an execution witness is incomplete.
- L1: `debug_getRawHeader`, `debug_getRawReceipts`, `eth_getBlockByHash`, and beacon blob
  sidecar fetches (small and bounded — only the L1 window spanning the range's L1 origin).

There is **no cross-request cache**: the Kona host KV store is in-memory (`data_dir: None`,
`online.rs:297`) and discarded after each build. The SP1 validity lane splits a range into
sub-ranges and builds each witness sequentially (`proofs/succinct/utils/host/src/validity.rs:93`),
so the L2 RPC cost scales with the entire block interval, and overlapping proof requests repeat
all the work.

The witness partitions cleanly by provider:

- **L2-state portion (expensive):** `debug_executePayload` execution witnesses (state MPT nodes,
  bytecode, keys), L2 headers, and L2 transaction tries. The boundary output roots are derived
  from the L2 headers (the `L2ToL1MessagePasser` storage root is the header `withdrawals_root`
  post-Isthmus). This is essentially all the data and all the sequential round-trips.
- **L1-derivation portion (cheap, bounded):** L1 headers, receipts, and blobs spanning the
  range's L1 origin window (≤ ~20 L1 blocks per `resolve_l1_head`).

## Core idea

A proof full-node, with an **opt-in CLI flag**, captures each block's execution witness during
live `newPayload` import — specifically on the `WorldChainDefaultContext` engine API path, where
the `BasicEngineValidatorBuilder<OpEngineValidatorBuilder>` engine validator
(`crates/node/src/context.rs:207,348`), behind the `FlashblocksEngineApiBuilder`
(`context.rs:206`), already **fully executes** the incoming block (a "full read" — it recomputes
the state root) using the node's `ConfigureEvm`. Because that execution leaves the full read +
write access set in the revm `State` cache, the witness is captured directly from it with **no
re-execution**. The per-block witness is stored in a
**bounded in-memory ring buffer**. A new RPC returns the aggregated **full L2 pre-image set** for
a range in one call. The Kona host pre-seeds its KV store from that response; only the bounded L1
portion still hits the network.

```
              ┌─────────────────────── World Chain L2 node (--witness.collect) ──────────┐
 engine       │  newPayload ─▶ WitnessCapturingEvmConfig (delegates to OpEvmConfig)       │
 newPayload ──┼─▶ execute block ─▶ ExecutionWitnessRecord::from_executed_state(&state)    │
              │       │                  │ (zero re-execution; live revm State cache)      │
              │       ▼                  ▼                                                  │
              │  commit block      provider.witness(hashed_state) ─▶ trie nodes            │
              │                          │                                                  │
              │                          ▼                                                  │
              │                    WitnessCache (bounded ring buffer, by block number)      │
              │                          │                                                  │
              │        debug_collectRangeWitness(start,end) ◀── RPC ──┐                    │
              └──────────────────────────┼─────────────────────────────────┼───────────────┘
                                         ▼                                  │
                                   RangeWitness  ──────────────────────────┘
                                         │
              ┌──────────────────────────┼─────────── Kona host (build_range_input) ───────┐
              │  pre-seed KV store with L2 pre-images                                       │
              │  run derivation/execution ─▶ L2 hints hit cache; only L1 hints hit network  │
              │  on KV miss ─▶ normal RPC fetch (graceful fallback)                          │
              └─────────────────────────────────────────────────────────────────────────────┘
```

### Key safety property

The Kona host fetches a pre-image only on a KV-store **miss**. Pre-seeding is therefore purely an
optimization: anything the node fails to supply is fetched over RPC exactly as today.
**Correctness never depends on the cache being complete — only speed does.** Additionally, every
pre-image is content-addressed (`check_preimage`, `preimage_store.rs:48`: keccak/sha256 of the
value must equal its key), so a node serving bulk pre-images cannot inject anything the guest
would accept that is not a genuine pre-image.

## Why executor-wrapping, not ExEx

`BundleState` (and therefore an ExEx `ChainCommitted` notification) only records *changed*
accounts/slots — it omits the read-only access set (SLOADs, account reads that did not mutate
state). The witness `keys` and trie-node set require the **full** touched set, which exists only
in the live revm `State.cache` during execution. So capturing a `debug`-identical witness without
re-execution requires wrapping the executor used during block import.

The repo's existing `BalBlockBuilder` / `FlashblocksBlockBuilder`
(`crates/evm/src/execution/{bal,basic}.rs`) sit on the **payload-building (sequencer)** path, not
the `newPayload` **import/validation** path that a proof full-node runs, so they are not a reuse
point here.

Constraint (per design decision): **do not modify the existing block-executor types**
(`OpEvmConfig` / `OpEvm`). The capturing layer is a new wrapper, installed only when the CLI flag
is set; when disabled it is a pure pass-through and the node behaves byte-identically to today.

## Component versions

- reth (paradigmxyz): tag `v2.3.0` (`9384bc5`).
- op-reth (ethereum-optimism/optimism fork): rev `423d93e`.
- `alloy_rpc_types_debug::ExecutionWitness { state, codes, keys, headers }` (all `Vec<Bytes>`),
  via `alloy-rpc-types-debug` `2.0.5`.
- `reth_revm::witness::ExecutionWitnessRecord` { `hashed_state`, `codes`, `keys`,
  `lowest_block_number` }; `record_executed_state` / `from_executed_state`.
- `StateProofProvider::witness(input, hashed_state, mode)` (`storage-api/src/trie.rs`) produces the
  trie-node `state` set by walking pre-state cursors for the touched hashed keys (no execution).

## Implementation plan

### Phase 1 — `crates/witness` (new crate): types + cache

- `WitnessCache`: a bounded ring buffer behind a `parking_lot::Mutex<…>`, keyed by block number,
  capacity from CLI.
  Reorg-safe (entries overwritten by block number; stale higher numbers dropped).
- `BlockWitness { execution_witness: ExecutionWitness, header_rlp: Bytes, transactions: … }`.
  No separate `L2ToL1MessagePasser` proof is stored: post-Isthmus the message-passer storage root
  is carried in the block header's `withdrawals_root`, so the output root
  `keccak(version ‖ state_root ‖ message_passer_storage_root ‖ block_hash)` is fully
  reconstructable from `header_rlp` alone. (The execution witness already includes the
  message-passer storage nodes, which reth force-loads post-Isthmus.)
- `RangeWitness` — the serde wire type returned by the RPC, aggregating the per-block witnesses for
  `(start, end]`.

### Phase 2 — capture wrapper (`crates/evm`)

Lives in `crates/evm` (next to the existing execution module) since it is EVM/reth-coupled;
`crates/witness` stays the minimal shared cache/wire layer.

- `WitnessCapturingEvmConfig`: a newtype over `WorldChainEvmConfig` holding an
  `Option<Sender<CapturedBlock>>`. Implements `ConfigureEvm` + `ConfigurePostExecEvm` by delegation.
  Its `type BlockExecutorFactory = WitnessBlockExecutorFactory` wraps the inner
  `OpBlockExecutorFactory`; `create_executor` returns a `WitnessExecutor` that delegates every
  method to the inner executor and, in `finish()`, snapshots
  `ExecutionWitnessRecord::from_executed_state(self.inner.evm().db(), mode)` from the live revm
  `State` cache (full read+write set, **zero re-execution**) and sends `CapturedBlock { block_number,
  record }` to the collector. When the sender is `None`, every path delegates straight through —
  exactly today's behavior.
- Capture is on the **validator** path only: `BasicEngineValidator<Node::Provider, Node::Evm, …>`
  (`reth … rpc.rs:1457,1472`) is hard-tied to `Node::Evm` and executes via
  `evm_config.create_executor` (`payload_validator.rs:1053`; the BAL path
  `bal/execute.rs:143` routes through the same factory). So making `Node::Evm` the wrapper captures
  all `newPayload` execution.
- `WorldChainExecutorBuilder` (`crates/evm/src/lib.rs:46`, installed at `context.rs:281`) outputs
  `WitnessCapturingEvmConfig` and, when collection is enabled, spawns the collector and installs the
  sender. The **block-builder path is unaffected**: it constructs `OpBlockExecutor` standalone
  (`crates/builder/src/payload_builder.rs:684`), not via the config factory. The only coupling is
  the `Ctx: PayloadBuilderCtx<Evm = WorldChainEvmConfig>` type bound, satisfied by unwrapping the
  inner config at the payload-service-builder boundary (`crates/node/src/payload.rs`). `Node::Evm`
  is thus always the wrapper but inert (sender `None`) unless the flag is set, so default behavior
  is byte-identical.
- **Collector** (`crates/evm`, spawned with the node's `Provider` + task executor): receives
  `CapturedBlock`s, runs the deferred trie walk `provider.state_by_block_id(parent)?.witness(…,
  record.hashed_state, mode)` for the `state` nodes (off the hot validation path), fills `headers`
  from `record.lowest_block_number`, reconstructs `header_rlp` + transactions from the provider by
  number, assembles the `BlockWitness`, and inserts it into the `WitnessCache`. Keyed by block
  number; reconciles to the committed hash and handles reorgs via cache overwrite.
- **Note:** when the BAL parallel path activates (post-Amsterdam), `execute_block_bal` bypasses
  `create_executor`; gate/alert on this so capture does not silently go dark.

### Phase 3 — CLI (`crates/cli`)

- `--witness.collect` (bool, default `false`) and `--witness.cache-size <N>` (default `1024`).
  Threaded through `WorldChainNodeConfig`. When set: the executor builder installs the capturing
  wrapper and the add-ons install the RPC.

### Phase 4 — RPC (`crates/rpc` + `crates/node/src/add_ons.rs`)

- New jsonrpsee method `debug_collectRangeWitness(start, end) -> RangeWitness` in the standard
  `debug` namespace. Reads the range from the cache and assembles the response. Installed only when
  collection is enabled, mirroring the `debug_ext` wiring at `crates/node/src/add_ons.rs:374` via
  `modules.merge_if_module_configured(RethRpcModule::Debug, witness_ext.into_rpc())?` (and
  `auth_module.merge_auth_methods(...)` if it should also be reachable on the auth server), so it
  follows the operator's existing `debug` module toggle.

### Phase 5 — host integration (`proofs/kona-host`)

- `OnlineHostConfig` (`online.rs:44`) gains an optional `witness_oracle_rpc` URL. When set,
  `build_range_input` fetches `debug_collectRangeWitness(start, end)`, pre-seeds the Kona host
  KV store with the returned L2 pre-images (keyed by keccak/code hash exactly as the host's hint
  handlers would), then runs the existing derivation/execution. **Graceful fallback** to full RPC
  fetching on any miss or RPC unavailability.

### Phase 6 — tests

- Unit: ring-buffer eviction & reorg overwrite; wrapper-disabled ≡ default behavior; `RangeWitness`
  serde round-trip.
- Integration (test-utils harness): run a node with `--witness.collect`, import blocks, call the
  RPC, feed the result through `PreimageStore::check_preimages`, and prove a small range
  end-to-end — comparing cache-seeded vs on-demand output for equality.

## Scope & rollout notes

- This touches node executor internals + a new RPC + CLI + the proof host. Implement phase by
  phase, compiling and testing each before moving on.
- The default node path (flag off) remains byte-identical to today.
- Suggested PR stacking: (1) crate + cache + wrapper, (2) RPC + CLI, (3) host integration.
- Two orthogonal wins, both realized here: **caching** (eliminates re-execution and the
  archive-state requirement on the proof path) and **batching** (one RPC call replaces N
  sequential round-trips and enables reuse across overlapping proof requests).
