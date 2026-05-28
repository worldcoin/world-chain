# OP Batcher ExEx — Specification

Status: **Draft** · Type: **Informational (engineering design)** · Created: **2026-05-28**

This document specifies an **OP Batcher** (L2 → L1 batch submitter) implemented as
a [reth ExEx](https://reth.rs/developers/exex/exex.html) (Execution Extension) running
**inside** the World Chain `op-reth` builder node. It is the direct sibling of the
existing **OP Proposer ExEx** (`crates/exex`, see [`src/lib.rs`](./src/lib.rs)) and is
intended to be implemented in the same crate (or a sibling crate `world-chain-op-batcher`)
following the same conventions.

The defining property of this design — and the reason it can exist as an ExEx at all —
is that **it reads the L2 chain (unsafe head, safe head, and per-block L1 origin)
directly from the in-process reth node state instead of from a remote `op-node`
`optimism_syncStatus` RPC.** The safe head marker in reth is advanced by `op-node`'s
derivation pipeline precisely as the batches this ExEx submits are derived back from
L1, which is exactly the signal the canonical batcher obtains from the rollup node's
`SyncStatus`. The batcher therefore needs no rollup RPC: only the local node provider
and an L1 execution-layer RPC (which it already needs to submit transactions).

---

## Upstream spec reference

All `Mirrors:` annotations in the implementation MUST be pinned to a single optimism
tag, the same way the proposer crate pins to `op-proposer/v1.16.3-rc.1`. This spec is
written against:

- **Implementation:** `github.com/ethereum-optimism/optimism` tag **`op-batcher/v1.16.7`**
  (`op-batcher/`, `op-node/rollup/derive/`, `op-service/txmgr/`, `op-service/eth/`).
- **Protocol:** `github.com/ethereum-optimism/specs` (`main`):
  - `specs/protocol/derivation.md` — batcher transaction / frame / channel / batch formats, channel-bank rules.
  - `specs/protocol/holocene/derivation.md` — strict (contiguous, ordered) frame/channel/batch handling.
  - `specs/protocol/granite/derivation.md` — `CHANNEL_TIMEOUT = 50`.
  - `specs/protocol/fjord/derivation.md` — brotli channel compression; larger channel-bank / RLP limits.
  - `specs/protocol/system-config.md`, `specs/protocol/configurability.md` — `batchInboxAddress`, `batcherHash`.

> **Version note.** The `op-batcher/v1.16.x` driver is the **multi-loop** design
> (`blockLoadingLoop` / `publishingLoop` / `receiptsLoop` / `throttlingLoop`) and uses
> `sync_actions.go::computeSyncActions`, which **replaced** the historical single
> `loop()` + `calculateL2BlockRangeToStore`. The implementation MAY mirror the simpler
> historical single-loop driver instead (lower complexity, no concurrent DA requests),
> but MUST state which upstream shape it mirrors in `lib.rs`, exactly as the proposer
> crate does. This spec describes the v1.16.7 semantics and calls out where a
> single-loop simplification is acceptable.

---

## Module → upstream file map

Mirrors the table at the top of the proposer crate's [`src/lib.rs`](./src/lib.rs). Proposed
internal modules for the batcher and their upstream Go counterparts:

| Internal module        | Upstream Go file(s)                                                            |
| ---------------------- | ----------------------------------------------------------------------------- |
| `batcher::config`      | `op-batcher/flags/flags.go`, `op-batcher/batcher/config.go`                    |
| `batcher::service`     | `op-batcher/batcher/service.go` (`BatcherServiceFromCLIConfig`, `initChannelConfig`) |
| `batcher::driver`      | `op-batcher/batcher/driver.go` (`BatchSubmitter`, loops, publish path)        |
| `batcher::sync`        | `op-batcher/batcher/sync_actions.go` (`computeSyncActions`) — **re-sourced from local state** |
| `batcher::channel_manager` | `op-batcher/batcher/channel_manager.go` (`channelManager`)                 |
| `batcher::channel_builder` | `op-batcher/batcher/channel_builder.go` (`ChannelBuilder`)                 |
| `batcher::channel`     | `op-batcher/batcher/channel.go` (`channel` — per-channel tx tracking)         |
| `batcher::channel_out` | `op-node/rollup/derive/channel_out.go`, `span_channel_out.go`, `singular_batch.go` |
| `batcher::frame`       | `op-node/rollup/derive/frame.go` (`Frame.MarshalBinary`, `FrameV0OverHeadSize`) |
| `batcher::tx_data`     | `op-batcher/batcher/tx_data.go` (`txData.CallData`, `txData.Blobs`)           |
| `batcher::da`          | `op-batcher/batcher/channel_config_provider.go` (`DynamicEthChannelConfig`)   |
| `batcher::compressor`  | `op-batcher/compressor/*.go` (`shadow`, `ratio`)                              |
| `batcher::provider`    | (no upstream) — alloy L1 provider; identical to proposer `src/provider.rs`     |
| `batcher::source`      | (no upstream) — local-state block reader; analogous to proposer `src/source/local.rs` |
| `batcher::metrics`     | `op-batcher/metrics/metrics.go`                                               |
| `batcher::rpc`         | `op-batcher/rpc/api.go` (`admin_startBatcher` / `admin_stopBatcher` / `admin_flushBatcher`) |
| `batcher::db`          | (no upstream — see [Persistence](#10-persistence)) — MDBX, mirrors proposer `src/db.rs` |

---

## 1. Scope, goals, and non-goals

### Goals

1. Run a fully functional pre-interop OP batcher in-process inside the World Chain
   `op-reth` builder, submitting batch transactions to the L1 `BatchInbox`.
2. Read all L2 data (blocks, unsafe head, safe head, per-block L1 origin) from the
   local reth node provider — **no `op-node` rollup RPC dependency**.
3. Support both **calldata** and **EIP-4844 blob** data availability, and the **auto**
   (cost-minimizing) DA mode.
4. Match the byte-for-byte wire formats the OP derivation pipeline expects (frames,
   channels, batcher-tx version prefix, singular/span batch encoding) so that batches
   produced by this ExEx are indistinguishable from `op-batcher` output.
5. Be enable/disable-gated exactly like the proposer ExEx (`--batcher.enabled`),
   draining ExEx notifications when disabled.

### Non-goals

- **Interop / supernode / supervisor** sources (the cross-safe `SafeL2`, super-roots,
  `op-supervisor`) are out of scope, mirroring the proposer crate's pre-interop slice.
- **AltDA / plasma** (`op-batcher` AltDA path in `driver.go::sendTransaction`).
- **Custom transaction manager.** As with the proposer, all nonce / gas / fee-bump /
  resubmission / confirmation concerns are delegated to the alloy wallet provider's
  filler stack (see [`src/provider.rs`](./src/provider.rs)), not a port of
  `op-service/txmgr`. The blob-fee escalation and `txpool.ErrAlreadyReserved`
  cancellation state machine of the Go txmgr are explicitly *not* ported in the first
  iteration; see [§9](#9-safety--correctness-constraints) and
  [Open questions](#open-questions).

---

## 2. Architecture overview

```text
                          op-reth node (World Chain builder)
   ┌───────────────────────────────────────────────────────────────────────┐
   │                                                                         │
   │   reth provider ──────────────┐                                         │
   │   (BlockReader + BlockIdReader)│  unsafe tip, safe head,                 │
   │                                │  block-by-number, L1 origin             │
   │                                ▼                                         │
   │   ExExNotifications ──►  Batcher ExEx body                               │
   │   (ChainCommitted/Reorged)     │  (keeps channel alive, persists head,   │
   │                                │   forwards FinishedHeight)              │
   │                                ▼                                         │
   │                        BatchSubmitter driver                            │
   │              ┌──────────────┬───────────────┬───────────────┐           │
   │              │ load blocks  │ channelManager │ publish txs    │          │
   │              │ (local read) │  → frames      │  → L1 provider │          │
   │              └──────────────┴───────────────┴───────┬────────┘          │
   └──────────────────────────────────────────────────── │ ──────────────────┘
                                                          ▼
                                          L1 EL RPC (alloy DynProvider, wallet)
                                                          │
                                                          ▼
                                          L1 BatchInbox  (calldata or blob tx)
                                                          │
                                                          ▼ derived by op-node
                                   reth safe head advances ⟲ (closes the loop)
```

The cycle is self-closing: the batcher submits unsafe blocks to L1; `op-node`'s
derivation pipeline reads the `BatchInbox`, validates the batch, and advances reth's
**safe head** via the Engine API; the batcher observes the new safe head locally and
prunes the now-safe blocks/channels. This is the local equivalent of the canonical
batcher polling `optimism_syncStatus` for `LocalSafeL2`.

### Local-state mapping (the core design decision)

The canonical batcher's only rollup-RPC dependencies are the inputs to
`getSyncStatus` / `computeSyncActions` / `safeL1Origin` / `waitNodeSync`
(`op-batcher/batcher/driver.go`, `sync_actions.go`). This ExEx re-sources every one of
them from local state:

| Canonical `eth.SyncStatus` field | Source in this ExEx | reth provider call |
| -------------------------------- | ------------------- | ------------------ |
| `UnsafeL2` (head to batch up to)  | reth canonical tip  | `best_block_number()` / `BlockIdReader::block_number_for_id`, or the latest `ExExNotification::ChainCommitted` tip |
| `LocalSafeL2` (safe reference)    | reth safe-block marker (set by op-node derivation via Engine API) | `BlockIdReader::safe_block_num_hash()` |
| `LocalSafeL2.L1Origin`            | L1-info deposit (first tx) of the safe L2 block | decode block 0-th tx (`L1BlockInfo`) — see [§5.3](#53-l1-origin-extraction) |
| `CurrentL1` / `HeadL1`            | L1 execution provider | `provider.get_block_number()` (alloy L1 `DynProvider`) |

`SafeL2` (cross-safe) is intentionally **not** used (interop only); the canonical batcher
itself uses `LocalSafeL2` here — see `sync_actions.go::computeSyncActions` (the comment
at the `LocalSafeL2` read). World Chain runs a single sequencer, so `LocalSafeL2` and the
canonical safe head coincide.

> **Why the safe head is the correct cursor.** The batcher MUST NOT re-batch blocks that
> are already safe (derived from L1), and MUST resume after a restart without
> double-submitting. The canonical batcher achieves both by deriving its resume point
> purely from `(LocalSafeL2, UnsafeL2)` every poll — it persists **no** last-batched
> cursor (`sync_actions.go`, "no blocks in state" branch). reth's safe head gives this
> ExEx the same authoritative, derivation-backed `LocalSafeL2` for free. The
> [persistence](#10-persistence) store is therefore a crash-recovery *aid*, not the
> source of truth, exactly as in the proposer crate.

---

## 3. ExEx body and lifecycle

Mirror the proposer's [`src/exex.rs`](./src/exex.rs) shape exactly.

```rust
pub async fn op_batcher_exex<N>(mut ctx: ExExContext<N>, cfg: BatcherConfig) -> Result<()>
where
    N: FullNodeComponents,
    N::Provider: ProviderBounds,
{
    // 1. Build the local block source over ctx.components.provider().
    // 2. Build BatcherService::from_config_with_source(cfg, source).await?
    // 3. service.start(&admin_settings).await?   // spawns the driver loop(s)
    // 4. Drain ExExNotifications: on each committed chain, persist head and
    //    send ExExEvent::FinishedHeight(tip.num_hash()).
}

pub async fn install_op_batcher_exex<N>(
    ctx: ExExContext<N>, args: BatcherCliArgs, fallback_datadir: PathBuf,
) -> Result<()> { /* if !args.enabled → drain_until_closed; else op_batcher_exex */ }
```

- **The ExEx body itself does no batching.** The `BatchSubmitter` driver runs in its own
  spawned task (Mirrors: `driver.go::StartBatchSubmitting` spawning the loops). The body
  only (a) keeps the notifications channel alive, (b) persists the latest committed L2
  head into the MDBX store, and (c) forwards `ExExEvent::FinishedHeight` so reth can
  prune. This matches `op_proposer_exex` line-for-line.
- **`ProviderBounds`** trait alias: `BlockReader<Header = Header> + BlockIdReader + Clone
  + Send + Sync + 'static` plus whatever transaction-body reads the block source needs
  (the proposer only needs headers; the batcher needs **full block bodies** — see
  [§5](#5-loading-l2-blocks-from-local-state)). Widen the bound to include
  `BlockReader` body access accordingly and document it on the alias, as
  [`src/local_node.rs`](./src/local_node.rs) documents its bounds.
- **ExEx reorg notifications** (`ChainReorged { old, new }`, `ChainReverted { old }`) are
  the in-process equivalent of `AddL2Block`'s `ErrReorg` parent-hash check
  (`channel_manager.go::AddL2Block`). The body SHOULD forward reorg notices to the driver
  (e.g. via a channel or a shared "rewind to N" flag) so the driver clears channel state
  and reloads from the safe head — see [§9](#9-safety--correctness-constraints).

---

## 4. Driver loop

Mirrors: `op-batcher/batcher/driver.go`.

### 4.1 Lifecycle

| Method                         | Mirrors (`driver.go`)                       |
| ------------------------------ | ------------------------------------------- |
| `BatchSubmitter::start()`      | `StartBatchSubmitting`                       |
| `BatchSubmitter::stop()`       | `StopBatchSubmitting`                        |
| `BatchSubmitter::stop_if_running()` | `StopBatchSubmittingIfRunning`          |
| `BatchSubmitter::flush()`      | `Flush` (force-close + submit open channel)  |

Use the same `parking_lot::Mutex<DriverState { running, cancel: CancellationToken,
stopped: Arc<Notify> }>` start/stop machinery as the proposer's
[`L2OutputSubmitter`](./src/driver.rs) (`tokio::select! { biased; cancel; tick }`,
`notify_one` on exit). Cancellation MUST win over a fired tick.

### 4.2 The loop(s)

The implementation MAY use either shape:

**(A) Single-loop (recommended first iteration; simpler).** One `tokio::time::interval`
ticking every `poll_interval`. Each tick:

1. `sync_status = self.local_sync_status().await?` — read `(unsafe_l2, safe_l2,
   safe_l1_origin, current_l1)` from local state + L1 provider ([§2](#local-state-mapping-the-core-design-decision)).
2. `actions = compute_sync_actions(&sync_status, prev_current_l1, &state)` — [§6](#6-sync-actions--restart-recovery).
3. Apply `Clear` / prune safe blocks / prune channels.
4. `load_blocks_into_state(actions.blocks_to_load)` — [§5](#5-loading-l2-blocks-from-local-state).
5. `publish_state_to_l1()` — drain ready tx data to L1 until `io::EOF` equivalent.

**(B) Multi-loop (faithful to v1.16.7).** `block_loading_loop` (ticks `poll_interval`,
signals `publish`), `publishing_loop` (consumes publish signals, owns the bounded
in-flight tx set and `MaxConcurrentDARequests` semaphore), `receipts_loop` (consumes
confirmations), optional `throttling_loop` (DA-size throttling via `miner_setMaxDASize`
on the local op-geth/builder — World Chain SHOULD NOT need this initially). Inter-loop
coordination via bounded (cap 1) channels, Mirrors: `driver.go` `publishSignal` /
`unsafeBytesUpdated`.

`wait_node_sync` (gated on `--batcher.wait-node-sync`) Mirrors `driver.go::waitNodeSync`:
block until the local safe head's L1 origin is within range of the L1 tip before the
first batching cycle. Mirrors the proposer's `wait_node_sync` shape.

### 4.3 Publish path

Mirrors: `driver.go::publishStateToL1` / `publishTxToL1` / `sendTransaction` / `sendTx`.

```text
loop {
    l1_tip = l1_provider.get_block_number()          // HeaderByNumber(latest)
    tx_data = channel_manager.tx_data(l1_tip)?        // None ⇒ break (io.EOF)
    match cfg.da_type effective for this channel {
        Calldata => calldata_tx_candidate(tx_data),   // exactly 1 frame (assert)
        Blobs    => blob_tx_candidate(tx_data),        // 1 blob per frame
    }
    send via alloy provider; on receipt → channel_manager.tx_confirmed(id, l1_inclusion)
                              on failure → channel_manager.tx_failed(id)
}
```

- **Destination:** `cfg.batch_inbox_address` for both calldata and blob txs
  (Mirrors `driver.go::blobTxCandidate` / `calldataTxCandidate`).
- **Sender:** the batcher EOA from the wallet provider. The L1 derivation pipeline only
  accepts batcher txs whose `from` matches the `SystemConfig.batcherHash`; the batcher
  does not set this, it must simply *be* the configured batcher key
  (`specs/protocol/system-config.md`). This is a **hard operational requirement**: the
  configured `--batcher.private-key` MUST correspond to the on-chain `batcherHash`.
- **Gas limit:** for calldata txs, set the gas limit to the EIP-7623 floor data gas
  (Mirrors `driver.go::sendTx` → `core.FloorDataGas`). Reuse / extend the proposer's
  `GasEstimateWithFallbackFiller` ([`src/tx.rs`](./src/tx.rs)) — note the 3/2 fallback
  margin there is tuned for DGF `create`; a batcher submitting a known-size calldata blob
  SHOULD compute the floor directly rather than relying on `eth_estimateGas`.
- **Confirmation / reorg safety:** the alloy provider's `PendingTransactionBuilder`
  (`with_required_confirmations`, `with_timeout`) plays the role the Go `txmgr`
  confirmation depth does. A submitted batch that is reorged out of L1 will *not* advance
  the safe head; the next `compute_sync_actions` cycle will see the blocks still unsafe
  and re-batch them — see [§9](#9-safety--correctness-constraints).

---

## 5. Loading L2 blocks from local state

Mirrors: `driver.go::loadBlocksIntoState` / `loadBlockIntoState`, with the
`EndpointProvider.EthClient(ctx).BlockByNumber(...)` RPC fetch **replaced by a local
provider read**.

### 5.1 Block source trait

Analogous to the proposer's `LocalStorageReader` ([`src/source/local.rs`](./src/source/local.rs)).

```rust
#[async_trait]
pub trait LocalBlockSource: Send + Sync {
    /// Full L2 block (header + all transactions) by number. The batcher needs
    /// the complete body: ChannelOut::add_block RLP-encodes every transaction.
    async fn l2_block(&self, number: u64) -> Result<OpBlock, BatcherSourceError>;
    /// Latest unsafe (canonical tip) and safe L2 block num+hash.
    async fn heads(&self) -> Result<LocalHeads, BatcherSourceError>;
}
```

The reth-backed adapter (analogous to `ExExChainReader` in
[`src/local_node.rs`](./src/local_node.rs)) wraps `ctx.components.provider()` and uses
`tokio::task::spawn_blocking` for the synchronous `BlockReader` calls. **`l2_block` MUST
return the full sealed block with recovered transactions**, because the channel encoder
RLP-encodes every transaction in the block (Mirrors `channel_out.go::AddBlock` /
`SpanChannelOut::AddBlock`).

### 5.2 Range loading

`load_blocks_into_state(start, end)` (inclusive) iterates `start..=end`, calls
`channel_manager.add_l2_block(block)` for each. Mirrors `loadBlocksIntoState`. Periodic
publish signalling (every ~100 blocks in upstream) MAY be omitted in the single-loop
variant since the same tick publishes immediately after loading.

### 5.3 L1 origin extraction

The L1 origin of an L2 block is encoded in its **first transaction**, the
`L1Block.setL1BlockValues*` / L1-info deposit. To compute `safe_l1_origin` (needed by
`compute_sync_actions` and `clear_state`), decode the L1-info deposit of the safe L2
block. Mirrors `derive.L2BlockToBlockRef` (`op-node/rollup/derive/l2block_util.go`) and
`L1BlockInfo` decoding (`op-node/rollup/derive/l1_block_info.go`). This is a pure local
decode — no RPC.

---

## 6. Sync actions & restart recovery

Mirrors: `op-batcher/batcher/sync_actions.go::computeSyncActions`, with `eth.SyncStatus`
replaced by the locally-assembled `LocalSyncStatus` from [§2](#local-state-mapping-the-core-design-decision).

`compute_sync_actions(new_status, prev_current_l1, state) -> (SyncActions, out_of_sync: bool)`:

1. **Empty-field / sequencer-restart guard.** If required fields are zero, or
   `CurrentL1` regressed below `prev_current_l1`, return `out_of_sync = true` and do
   nothing this cycle. (Mirrors `sync_actions.go` empty-field + reversal guards.)
2. **Unsafe range** = `[safe_l2.number + 1, unsafe_l2.number]`.
3. **No blocks in state** ⇒ `blocks_to_load = unsafe_range`. **This is the restart-
   recovery path**: after a process restart the channel manager is empty, so the batcher
   simply (re)loads every unsafe block above the derivation-backed safe head and resumes.
4. **Prune below safe head:** `blocks_to_prune = (safe_l2.number + 1) - oldest_block_in_state`.
   Already-safe blocks/channels are dropped (`PruneSafeBlocks` / `PruneChannels`).
5. **Reorg / safe-head-above-state / safe-chain reorg** ⇒ `start_afresh`: `Clear` channel
   state to `safe_l2.l1_origin` and reload the full unsafe range.
6. **Holocene stall detection** ⇒ `start_afresh`: if a channel is fully submitted, not
   timed out, `current_l1 > channel.max_inclusion_block`, yet `safe_l2` has not advanced
   past the channel's latest L2 block, assume the derivation pipeline dropped the channel
   under Holocene strict ordering and resubmit from the safe head. (Mirrors
   `sync_actions.go` Holocene stall branch — see [§11](#11-holocene-strict-ordering).)
7. **Happy path** ⇒ `{ blocks_to_prune, channels_to_prune, blocks_to_load:
   [newest_in_state + 1, unsafe_l2] }`.

`clear_state` Mirrors `driver.go::clearState`/`safeL1Origin`: set
`l1_origin_last_submitted_channel` from `safe_l2.l1_origin` (or the rollup
`Genesis.L1` if the safe origin is 0).

---

## 7. Channel manager, builder, and channel

### 7.1 channelManager

Mirrors: `op-batcher/batcher/channel_manager.go`.

- **One open channel at a time** (`current_channel`), a `channel_queue: Vec<Channel>`,
  a `blocks` queue of not-yet-safe `SizedBlock`s, a `block_cursor` index into `blocks`
  (next block to add to a channel), `l1_origin_last_submitted_channel`, and a
  `tx_channels: HashMap<TxId, ChannelIdx>` for routing receipts.
- `add_l2_block(block)` — reorg check (`tip_hash != block.parent_hash` ⇒ `ErrReorg`),
  enqueue as `SizedBlock`. (Mirrors `AddL2Block`.) In the ExEx, the `ErrReorg` path is
  also reachable directly from an `ExExNotification::ChainReorged`.
- `tx_data(l1_head)` — return the next ready `txData`; if no frame has been submitted for
  the current channel yet **and** the effective DA type would flip (auto mode), invalidate
  and rebuild the channel with the new config (Mirrors `TxData` + `handleChannelInvalidated`).
- `get_ready_channel` / `ensure_channel_with_space` / `process_blocks` / `output_frames`
  — Mirror the same-named Go methods. `process_blocks` adds blocks from `block_cursor`
  until the channel returns `ChannelFullError`. `output_frames` calls
  `ChannelBuilder::output_frames` and, on full, logs the achieved compression ratio and
  frees the compressor.
- `prune_safe_blocks` / `prune_channels` / `clear(l1_origin)` — Mirror the same.

### 7.2 ChannelBuilder

Mirrors: `op-batcher/batcher/channel_builder.go`.

Owns a `ChannelOut` (the compressing batch writer — [§8](#8-channel--frame--batch-encoding)),
the produced `frames` queue, a `frame_cursor`, and the combined channel `timeout`.

**`add_block(block)`** RLP-encodes the block into the `ChannelOut`; marks the channel
full on `ErrTooManyRLPBytes` (`MaxRLPBytesPerChannel`) or `ErrCompressorFull`; tracks
oldest/latest L1 origin and L2 block; updates the sequencer-window timeout.

**Timeouts** — the builder closes the channel at the *earliest* of:

| Timeout                | Formula (`channel_builder.go`)                                    | Full reason            |
| ---------------------- | ----------------------------------------------------------------- | ---------------------- |
| On-chain channel timeout | `frame_published_l1 + ChannelTimeout − SubSafetyMargin`         | `ErrChannelTimeoutClose` |
| Max channel duration   | `first_l1 + MaxChannelDuration` (disabled if `MaxChannelDuration == 0`) | `ErrMaxDurationReached` |
| Sequencer-window close | `l1_info_number + SeqWindowSize − SubSafetyMargin`                | `ErrSeqWindowClose`     |

Plus the size/encoding limits: `ErrCompressorFull`, `MaxRLPBytesPerChannel`,
`ErrMaxFrameIndex` (frame number is `uint16`).

**`output_frames`** — if the channel is full, `close()` the `ChannelOut` and drain all
frames (`close_and_output_all_frames`). If not full, eagerly emit a frame whenever
`ready_bytes + FrameV0OverHeadSize >= MaxFrameSize` (`output_ready_frames`) so frames
stream to L1 while the channel is still collecting blocks. Each emitted frame is a
`frameData { id: frameID { channel_id, frame_number }, data }`.

### 7.3 channel (per-channel tx tracking)

Mirrors: `op-batcher/batcher/channel.go`. Wraps a `ChannelBuilder` and tracks
`pending_transactions`, `confirmed_transactions`, `min/max_inclusion_block`.

- `next_tx_data()` — packs up to `max_frames_per_tx()` frames into one `txData`
  (**1** for calldata, `TargetNumFrames` for blobs).
- `has_tx_data()` — calldata: any pending frame; blobs: only once
  `pending_frames() >= max_frames_per_tx()` (or the channel is full).
- `is_timed_out()` — `max_inclusion_block − min_inclusion_block >= ChannelTimeout`
  (on-chain timeout detection).
- `tx_confirmed(id, inclusion_block)` — record confirmation, call
  `frame_published(inclusion_block)` to arm the on-chain timeout; returns whether the
  channel timed out on chain. `tx_failed(id)` — requeue the failed tx's frames, rewinding
  the `frame_cursor` to the **first** frame of the failed tx to preserve contiguous
  ordering ([§11](#11-holocene-strict-ordering)).

---

## 8. Channel / frame / batch encoding

These formats are consensus-critical: bytes produced here are parsed by the L1
derivation pipeline. They MUST match exactly. Mirrors: `op-node/rollup/derive/`.

### 8.1 Frame wire format

`op-node/rollup/derive/frame.go::Frame.MarshalBinary`:

```text
frame = channel_id (16 bytes) ‖ frame_number (uint16, BE) ‖ frame_data_length (uint32, BE)
        ‖ frame_data ‖ is_last (1 byte: 0x00 | 0x01)
```

- `FrameV0OverHeadSize = 23` (`channel_out.go`). `MaxFrameLen = 1_000_000` (`frame.go`).
- `ChannelID` is 16 bytes / 128-bit (`ChannelIDLength = 16`, `derive/params.go`).

### 8.2 Batcher transaction format

The L1 tx payload (calldata, or per-blob payload) is **version-prefixed**:

```text
payload = DerivationVersion0 (0x00) ‖ frame[0] [‖ frame[1] ‖ …]
```

`DerivationVersion0 = 0` (`op-node/rollup/derive/params/versions.go`). Mirrors
`tx_data.go::txData.CallData` (calldata: version byte then *all* frames of the single-
frame tx) and `txData.Blobs` (one blob per frame, each blob = `Blob::from_data(version
byte ‖ frame.data)`).

### 8.3 Batch encoding (singular vs span)

- Default `--batcher.batch-type = 0` (**SingularBatch**) — Mirrors `flags.go`.
- `SpanBatch` (`1`) uses `derive::SpanChannelOut` which compresses incrementally and
  supports `MaxBlocksPerSpanBatch`. Mirrors `op-node/rollup/derive/span_channel_out.go`,
  `singular_batch.go`, `span_batch.go`. Span batches are the production default on modern
  OP chains; the implementation SHOULD support both behind `batch_type`.
- A reth/Rust implementation SHOULD reuse the `kona` / `op-alloy` derive primitives
  (`ChannelOut`, `SingularBatch`, `SpanBatch`, `Frame`) if available rather than porting
  the encoders by hand — they are the canonical Rust implementations of these specs.

### 8.4 Channel-bank limits (must be respected by the producer)

From `op-node/rollup/chain_spec.go` and `specs/protocol/derivation.md` — the batcher MUST
NOT produce channels that exceed what the channel bank will accept:

| Limit                  | Bedrock        | Fjord+          |
| ---------------------- | -------------- | --------------- |
| `MaxChannelBankSize`   | 100_000_000    | 1_000_000_000   |
| `MaxRLPBytesPerChannel`| 10_000_000     | 100_000_000     |
| `ChannelTimeout`       | `ChannelTimeoutBedrock` (rollup cfg) | **50** (Granite, `ChannelTimeoutGranite`) |
| `MaxSpanBatchElementCount` | 10_000_000 | 10_000_000      |

The per-frame channel-bank accounting uses `frame_size = len(data) + frameOverhead(200)`
(`derive/params.go`). `ChannelBuilder` enforces `MaxRLPBytesPerChannel` while writing.

---

## 9. Data availability (calldata / blobs / auto)

Mirrors: `op-batcher/flags/types.go`, `batcher/service.go::initChannelConfig`,
`batcher/channel_config_provider.go`, `batcher/tx_data.go`.

- `DataAvailabilityType ∈ { Calldata, Blobs, Auto }`; flag default `calldata`.
- **Calldata config:** `MaxFrameSize = MaxL1TxSize − 1`, `MaxFramesPerTx = 1`,
  `UseBlobs = false`. `MaxL1TxSize` default 120_000.
- **Blobs config:** `MaxFrameSize = eth.MaxBlobDataSize − 1`, `MaxFramesPerTx =
  TargetNumFrames`, `UseBlobs = true`. Requires Ecotone. `TargetNumFrames` MUST be
  `≤ MaxBlobsPerBlock` (one blob per frame). `eth.MaxBlobDataSize = (4*31 + 3)*1024 − 4`
  (`op-service/eth/blob.go`).
- **Auto:** holds both a blob config and a hardcoded calldata fallback
  (`TargetNumFrames = 1, MaxFrameSize = 120_000`) in a `DynamicEthChannelConfig`. Per the
  `tx_data` call (only before the first frame of a channel is submitted), it picks the
  cheaper of a single calldata tx vs a single blob tx using suggested L1 gas / blob-gas
  prices and the EIP-7623 calldata floor; while throttling it always prefers blobs.
  Mirrors `channel_config_provider.go::DynamicEthChannelConfig.ChannelConfig`.

`MaxDataSize(num_frames, max_frame_size) = num_frames * (max_frame_size −
FrameV0OverHeadSize)` becomes the compressor `TargetOutputSize`
(`channel_config.go::InitCompressorConfig`).

### Configuration parameters & defaults

| Parameter            | CLI flag (proposed)             | Default        | Mirrors |
| -------------------- | ------------------------------- | -------------- | ------- |
| Enable               | `--batcher.enabled`             | `false`        | (ExEx gate, like proposer) |
| L1 RPC               | `--batcher.l1-eth-rpc`          | —              | `flags.go L1EthRpc` |
| BatchInbox address   | `--batcher.batch-inbox-address` | — (from rollup cfg) | `RollupConfig.BatchInboxAddress` |
| DA type              | `--batcher.data-availability-type` | `calldata`  | `flags.go` |
| Target num frames    | `--batcher.target-num-frames`   | `1`            | `flags.go` |
| Max L1 tx size bytes | `--batcher.max-l1-tx-size-bytes`| `120000`       | `flags.go` |
| Max channel duration | `--batcher.max-channel-duration`| `0` (disabled) | `flags.go` |
| Sub-safety margin    | `--batcher.sub-safety-margin`   | `10`           | `flags.go` |
| Poll interval        | `--batcher.poll-interval`       | `6s`           | `flags.go` |
| Max pending txs      | `--batcher.max-pending-tx`      | `1`            | `flags.go` |
| Compressor           | `--batcher.compressor`          | `shadow`       | `flags.go` |
| Approx compr ratio   | `--batcher.approx-compr-ratio`  | `0.6`          | `flags.go` |
| Compression algo     | `--batcher.compression-algo`    | `zlib`         | `flags.go` (brotli requires Fjord) |
| Batch type           | `--batcher.batch-type`          | `0` (singular) | `flags.go` |
| Wait node sync       | `--batcher.wait-node-sync`      | `false`        | `flags.go` |
| Signer (key/mnemonic)| `--batcher.private-key` / `--batcher.mnemonic` | — | provider (like proposer) |
| Datadir              | `--batcher.datadir`             | `<datadir>/op-batcher` | — |
| Admin RPC addr/port  | `--batcher.rpc-addr` / `--batcher.rpc-port` | `127.0.0.1` / `0` | `rpc/api.go` |

`BatcherCliArgs` MUST implement a **manual `Debug` that redacts `private_key` /
`mnemonic`**, exactly as `ProposerCliArgs` does ([`src/config.rs`](./src/config.rs)).
`into_config` validates: L1 RPC present, batch-inbox present, exactly one signer,
`TargetNumFrames ≤ MaxBlobsPerBlock` for blob/auto DA, brotli only post-Fjord.

---

## 10. Compression

Mirrors: `op-batcher/compressor/*.go`.

- `Kind ∈ { Ratio, Shadow, None }`; default **shadow**. `ApproxComprRatio` default 0.6.
- **Shadow** runs a parallel zlib writer to *guarantee* output stays under
  `TargetOutputSize` (preferred — never overshoots a frame/blob budget). **Ratio** assumes
  the configured ratio and may overshoot.
- `CompressionAlgo ∈ { Zlib, Brotli(9|10|11) }`. Brotli is gated on Fjord activation.
- Singular batches use `SingularChannelOut(compressor)`; span batches embed compression in
  `SpanChannelOut`.

---

## 11. Safety & correctness constraints

A conformant batcher (canonical or this ExEx) MUST satisfy:

1. **Never batch beyond the unsafe head.** The load range is always capped at
   `unsafe_l2.number` (`sync_actions.go`).
2. **Channel landing margin.** `SubSafetyMargin` (default 10) is subtracted from both the
   `ChannelTimeout` and `SeqWindowSize` close timeouts so the *entire* channel lands on
   L1 — and is derivable — before the consensus channel timeout / sequencing window
   expires. A channel whose frames straddle the on-chain timeout is dropped by the channel
   bank and must be resubmitted.
3. **L2 reorg handling.** `add_l2_block` parent-hash mismatch ⇒ `ErrReorg`; the driver
   then clears channel state and reloads from the safe head. In the ExEx, an
   `ExExNotification::ChainReorged`/`ChainReverted` MUST trigger the same clear-and-reload
   (the in-process counterpart of `waitNodeSyncAndClearState`). Because the batcher's
   resume cursor is the derivation-backed **safe head**, a reorg of *unsafe* blocks simply
   results in the new unsafe blocks being (re)batched on the next cycle.
4. **Safe head advances as batches derive back.** The batcher relies on reth's safe head
   advancing once op-node derives the submitted channel. Until it does, the blocks remain
   in the unsafe range and are re-loaded; the persistence store is **not** the source of
   truth. Pruning happens only once blocks are safe.
5. **Restart recovery without a persisted cursor.** On restart the channel manager is
   empty; `compute_sync_actions` reloads `[safe_l2 + 1, unsafe_l2]`. No double-submission
   occurs because already-derived blocks are at or below the safe head.
6. **Reorged-out L1 batch.** A batch tx that is reorged out of L1 does not advance the
   safe head; its L2 blocks stay unsafe and are re-batched. The alloy provider's required-
   confirmations setting is the analogue of the Go txmgr confirmation depth.
7. **DA-type switch only pre-submission.** In auto mode the blobs↔calldata switch happens
   only before any frame of the current channel is on L1 (`channel_manager.go::TxData`).
8. **Secrets.** The batcher EOA key MUST be redacted in all `Debug`/log output. No secrets
   in code, logs, or metrics labels (project rule).

> **txmgr gap.** Because this design delegates to the alloy filler stack rather than
> porting `op-service/txmgr`, the **blob-fee / tip escalation on stuck txs** and the
> `txpool.ErrAlreadyReserved` cancellation state machine are not present in the first
> iteration. For a single-sequencer World Chain builder with `max-pending-tx = 1` this is
> acceptable (each batch tx is awaited to a receipt before the next is sent), but it is a
> known limitation under L1 fee spikes and is tracked in [Open questions](#open-questions).

---

## 12. Holocene strict ordering

Spec: `specs/protocol/holocene/derivation.md`. Holocene makes frame/channel/batch handling
**strictly ordered and contiguous**: out-of-order or gapped frames/channels/batches are
*dropped* rather than buffered, and the safe head advances only on a contiguous valid
sequence. The batcher already satisfies this by construction:

- **Single open channel + in-order frame emission** (`frame_cursor`, `next_frame` /
  `rewind_frame_cursor`).
- **`tx_failed` rewinds to the first frame of the failed tx**, never leaving a gap
  (`channel.go`).
- **`handle_channel_invalidated` drops the channel *and all newer channels*** and rewinds
  `block_cursor` to the channel's first block (`channel_manager.go`).
- **Holocene stall recovery** in `compute_sync_actions` ([§6](#6-sync-actions--restart-recovery),
  step 6): a fully-submitted, non-timed-out channel whose blocks fail to become safe after
  its max inclusion block is assumed dropped by strict ordering, and the batcher starts
  afresh from the safe head.

---

## 13. Node wiring

Mirrors the proposer wiring in [`crates/node/src/extensions.rs`](../node/src/extensions.rs)
and the CLI in [`crates/cli/src/cli.rs`](../cli/src/cli.rs):

```rust
// crates/cli/src/cli.rs
#[command(flatten)]
pub batcher: BatcherCliArgs,

// crates/node/src/extensions.rs
fn install_extensions(self, args: &WorldChainArgs) -> Self {
    let proposer_datadir = self.config().datadir().data_dir().join("op-proposer");
    let batcher_datadir  = self.config().datadir().data_dir().join("op-batcher");
    let s = install_op_proposer(self, args.proposer.clone(), proposer_datadir);
    install_op_batcher(s, args.batcher.clone(), batcher_datadir)
}

fn install_op_batcher(builder, args, fallback_datadir) -> ... {
    builder.install_exex_if(args.enabled, "op-batcher", move |ctx| async move {
        Ok(async move { install_op_batcher_exex(ctx, args, fallback_datadir).await
            .map_err(eyre::eyre::Report::from) })
    })
}
```

The `ProviderBounds` bound on `NodeAdapter<...>::Provider` MUST be widened (or a new
`BatcherProviderBounds` introduced) to cover full-block-body reads in addition to the
header/block-id reads the proposer needs.

---

## 14. Persistence

Mirrors the proposer's [`src/db.rs`](./src/db.rs) MDBX store (single-entry tables, JSON
values, `parking_lot::Mutex` write serialization). For the batcher this is a **crash-
recovery aid only** (the safe head is the real cursor — [§11](#11-safety--correctness-constraints)):

- `head` table: last L2 head the ExEx finished processing `(block_number, block_hash)`
  (identical to proposer `StoredHead`; used for `ExExEvent::FinishedHeight` bookkeeping).
- `last_batch` table (optional): last successfully confirmed batch tx
  `(l2_range, channel_id, l1_tx_hash, l1_block_number, at_unix)` — for observability and
  to short-circuit re-submission of an in-flight channel across a fast restart. It MUST
  NOT be treated as authoritative over the derivation-backed safe head.

---

## 15. Metrics & admin RPC

- **Metrics** (Mirrors `op-batcher/metrics/metrics.go`; keep an `op_batcher_*` scope so
  dashboards port over): `up`, `last_batch_l2_block`, `batch_submissions`,
  `batch_failures`, `channel_full_total{reason}`, `pending_blocks_count`,
  `pending_blocks_bytes_total`, `blob_used_bytes` / `calldata_used_bytes`,
  `wallet_balance_eth` (reuse the proposer's `spawn_balance_poller`,
  [`src/metrics.rs`](./src/metrics.rs)).
- **Admin RPC** (Mirrors `op-batcher/rpc/api.go`; namespace `admin`, jsonrpsee, like
  [`src/rpc.rs`](./src/rpc.rs)): `admin_startBatcher`, `admin_stopBatcher`,
  `admin_flushBatcher` (force-close + submit the open channel). Disabled when
  `--batcher.rpc-port = 0`.

---

## 16. Test plan

1. **Encoding conformance** (unit): frame `MarshalBinary` byte-layout, version-prefixed
   tx payload, singular/span batch round-trips. Cross-check against `op-alloy`/`kona`
   golden vectors. (Cf. the proposer's `output_root_layout_matches_op_v0` test in
   [`src/source/local.rs`](./src/source/local.rs).)
2. **Channel state machine** (unit): channel full on each `FullReason`, timeout ordering
   (earliest wins), `tx_failed` frame-cursor rewind, DA-type switch pre-submission only.
3. **`compute_sync_actions`** (unit, table-driven): no-blocks-in-state, happy path,
   prune-below-safe, reorg/start-afresh, Holocene stall — mirror upstream
   `sync_actions_test.go`.
4. **E2E in the devnet** (Mirrors the proposer's `tests/proposer_e2e.rs`): bring up the
   World Chain devnet with the batcher ExEx enabled and the external `op-batcher`
   disabled; assert the safe head advances, batches land in the `BatchInbox`, and a
   restarted batcher resumes from the safe head without gaps or double-submission. Verify
   both calldata and blob DA modes.

---

## Open questions

1. **Port `op-service/txmgr`?** The first iteration relies on the alloy filler stack +
   `max-pending-tx = 1`. Under sustained L1 fee spikes a stuck batch tx will block
   progress without fee escalation. Decide whether to port the txmgr escalation /
   cancellation state machine (§11 txmgr gap) before mainnet.
2. **Throttling.** Is DA-size throttling (`miner_setMaxDASize` against the local builder)
   needed for World Chain, given the builder and batcher are co-located? Likely yes under
   load; the `throttling_loop` can be added incrementally.
3. **Single vs multi-loop driver.** Start single-loop for simplicity, or go straight to
   the v1.16.7 multi-loop with concurrent DA requests? Recommendation: single-loop first,
   matching the proposer's complexity budget.
4. **Reuse `kona` derive primitives** for `ChannelOut` / batch encoding vs. hand-porting?
   Strongly prefer reuse to avoid a consensus-critical re-implementation.
5. **Interaction with the external `op-batcher`.** During rollout both may run; the design
   assumes the external batcher is *disabled* when the ExEx is enabled (two batchers from
   the same `batcherHash` would double-submit and waste L1 gas). Enforce operationally.
