# world-chain-acceptance

A code-derived acceptance framework for World Chain. Tests live in code and
self-register; the runner selects and gates them per a committed manifest,
executes them against a provisioned target (a spawned devnet or a remote
network), and emits a report. It is designed to be run against a devnet
configuration to surface bugs before cutting a release candidate.

The design borrows the strongest ideas from Optimism's `op-devstack`, Base's
action harness, and Tempo's e2e harness:

- **Declarative requirements** (improving on OP's imperative `IsForkActive`
  checks): each test states the hardfork/features it needs, so the runner can
  select, gate, and report coverage without running the body.
- **Gate semantics**: an unmet precondition skips, with an env override that
  flips skip → fail when an orchestrator expected the test to run.
- **Target seam**: the runner is oblivious to whether the `Env` is a spawned
  Docker devnet or a remote endpoint, so one suite runs against both.
- **Fork-matrix sweep** (Base's `ForkMatrix`): run the suite across a sequence
  of hardforks, one provisioned devnet per cell, aggregated into one report.

## Architecture

```
catalog + Requirements (data)   ← what each test needs (fork / features / serial)
        │
runner (selection, gating, concurrency, matrix aggregation)
        │
AcceptanceTarget (composer seam)  ← provisions an Env + async teardown
   ├─ Remote        (in-crate; RPC to a deployed network)
   ├─ SpawnedDevnet (in xtask; drives world_chain_devnet)   [primary]
   └─ InProcess     (reserved seam — fast reth-e2e tier, not implemented)
        │
Env / TestCtx (typed per-test frontend: hands tests providers, not URLs)
```

Each test body is annotated with `#[acceptance_test]`, which registers it in a
link-time catalog (via `inventory`). There is no external gate or category file
to keep in sync — the catalog is derived from the test code.

## Authoring a test

```rust,ignore
use std::sync::Arc;
use world_chain_acceptance::{TestCtx, acceptance_test};

// Applies to every committed network.
#[acceptance_test]
async fn rpc_is_live(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    let chain_id = ctx.chain_id().await?;
    ctx.record_i64("chain_id", chain_id as i64);
    Ok(())
}

// Only selected when the manifest commits to flashblocks; reported as skipped
// otherwise.
#[acceptance_test(features = ["flashblocks"])]
async fn flashblocks_streams(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    Ok(())
}

// Only selected once the network commits to Tropo or later.
#[acceptance_test(requires_hardfork = "tropo", serial)]
async fn tropo_behavior(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    Ok(())
}
```

`#[acceptance_test]` arguments (all optional):

| Argument | Meaning |
| --- | --- |
| `name = "..."` | Stable test name (defaults to the function name). |
| `requires_hardfork = "tropo"` | Minimum `WorldChainHardfork`; skipped on earlier forks. |
| `features = ["flashblocks", "pbh"]` | Required features; skipped unless the manifest commits to all of them. |
| `serial` | Opt out of intra-cell parallelism (for tests that own chain state). |
| `flaky = "reason"` | Failures downgrade to skips unless `ACCEPTANCE_FAIL_FLAKY_TESTS=true`. |

The runner provides framework-level helpers on the context: typed providers
(`ctx.l2()`, `ctx.engine()`), poll helpers (`ctx.wait_for_block`,
`ctx.wait_for_blocks`, `ctx.await_receipt`), an isolated per-test signer
(`ctx.signer(i)`), metric recording (`ctx.record_*`), and run-time skips
(`ctx.skip` / `ctx.skip_if`).

## Manifest schema

The target manifest schema lives in `world-chain-chainspec::manifest`:

```toml
name = "alphanet"
hardfork = "jovian"
features = ["flashblocks", "block_access_list", "pbh"]

[chain]
spec = "dev"
# genesis = "genesis.json"
chain_id = 2151908
```

## Running

```sh
# Single-cell run against a freshly spawned devnet derived from the manifest.
cargo run -p xtask -- acceptance \
  --manifest manifests/devnet.toml \
  --report report.json --markdown report.md --junit report.xml

# Fork-matrix sweep: one provisioned devnet per fork from genesis through Strato.
cargo run -p xtask -- acceptance \
  --manifest manifests/devnet.toml --fork-matrix through:strato

# Remote network over RPC (single deployed fork; --fork-matrix is rejected).
cargo run -p xtask -- acceptance \
  --manifest manifests/alphanet.toml --target alphanet --rpc-url "$RPC_URL"

# Filter by package/name.
cargo run -p xtask -- acceptance \
  --manifest manifests/devnet.toml --package tests::spec --name chain_id
```

`--fork-matrix` accepts `all`, `through:<fork>`, or a comma list
(`jovian,tropo,strato`). The command prints a summary, optionally writes
JSON/Markdown/JUnit reports, and exits non-zero if any non-flaky executed test
fails.

### Environment variables

| Variable | Effect |
| --- | --- |
| `ACCEPTANCE_FAIL_FLAKY_TESTS` | Make flaky tests fail normally instead of downgrading to skips. |
| `ACCEPTANCE_EXPECT_PRECONDITIONS_MET` | Promote a run-time `ctx.skip` into a failure (declarative pre-gating still skips). For orchestrators that selected a test and expect it to run. |
| `ACCEPTANCE_CONCURRENCY` | Max non-serial tests run concurrently within a cell (default 4). |
| `ACCEPTANCE_KEYS_SALT` | Salt for per-test signer derivation; reproducible account isolation. |

Connection inputs (`ACCEPTANCE_RPC_URL`, `ACCEPTANCE_L1_RPC_URL`,
`ACCEPTANCE_ENGINE_RPC_URL`, `ACCEPTANCE_ENGINE_JWT`,
`ACCEPTANCE_FLASHBLOCKS_URL`, `ACCEPTANCE_PROMETHEUS_URL`, and
`CF_ACCESS_CLIENT_ID` / `CF_ACCESS_CLIENT_SECRET`) double as CLI flags. The
Engine JWT and Cloudflare secret are never logged.

## Coverage matrix

The framework gates tests by fork and feature; this table tracks which axes have
real coverage versus framework placeholders. Update it as tests land.

| Axis | Status | Notes |
| --- | --- | --- |
| Core spec (chain id, engine handshake) | placeholder | `tests::spec` — chain id + `engine_exchangeCapabilities`. |
| `flashblocks` feature | placeholder | endpoint-reachability stub; stream assertions TODO. |
| `block_access_list` feature | none | TODO. |
| `pbh` feature | none | TODO. |
| Hardforks through Jovian | none | exercised structurally by the fork-matrix; assertions TODO. |
| Tropo hardfork | placeholder | gating stub only. |
| Strato hardfork | none | TODO. |

## Tiers

The primary tier is a spawned `world_chain_devnet` (Docker), with a remote-RPC
tier for deployed networks. `Tier::InProcess` is a reserved seam for a future
fast, deterministic reth-e2e tier (drive blocks directly, no Docker) modeled on
Tempo's integration harness and Base's action harness — not yet implemented.
