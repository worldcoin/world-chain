# world-chain-acceptance

A modular, manifest-driven acceptance harness for World Chain. A committed
[`NetworkManifest`](../manifest) is the single source of truth: the node derives
its chain spec from it (`--chain manifest.toml`) and the harness uses the same
file to decide which checks run and to assert the live network delivers
everything it commits to. The harness asserts network-wide **health**, **spec
compatibility**, and **performance**, and emits an acceptance report.

## The commitment model

A manifest commits to a canonical chain spec (a named OP/World spec or a
[`Genesis`] file) plus World Chain features. Committed *requirements* are read
back from that chain spec via reth's canonical hardfork traits, plus the enabled
feature keys — they are not re-declared by the harness.

A check declares the requirement keys it needs via `requires(...)`:

- if the manifest commits to all of them, the check runs and verifies the live
  network;
- if not, the check is **skipped** (not failed);
- if a key names no known fork or feature for the committed spec, it is a
  declaration error (failed).

The report flags any committed requirement that no check exercises, so a
deployment cannot quietly commit to functionality nothing tests. `--strict`
turns that into a hard failure.

## Adding a check

Write an `async fn` taking the [`TestCtx`] and annotate it with
`#[acceptance_test]`. It self-registers at link time via [`inventory`].

```rust,ignore
use std::sync::Arc;
use world_chain_acceptance::{TestCtx, acceptance_test};

// Always runs.
#[acceptance_test(category = Health)]
async fn rpc_is_live(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    let chain_id = ctx.chain_id().await?;
    ctx.record_i64("chain_id", chain_id as i64); // surfaces in the report
    Ok(())
}

// Runs only when the manifest commits to the `flashblocks` feature.
#[acceptance_test(category = SpecCompatibility, requires(flashblocks))]
async fn flashblocks_capability(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    Ok(())
}

// Runs only when the committed chain spec schedules the Jovian hardfork.
#[acceptance_test(category = SpecCompatibility, requires(jovian))]
async fn jovian_header_format(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    Ok(())
}
```

Requirement keys are hardfork names (lowercase, e.g. `jovian`, `cancun`) or
feature keys (`flashblocks`, `flashblocks_access_list`, `pbh`). `ctx` derefs to
[`Env`], so RPC helpers like `ctx.chain_id()` and `ctx.supported_capabilities()`
are available directly; use `ctx.record_*` to attach report metrics and
`ctx.skip_if(cond, reason)` to skip at run time when an optional endpoint is
absent.

## The executor

How an individual check is executed is abstracted behind the [`TestExecutor`]
trait, so isolation/parallelism strategy is pluggable. The default
[`PanicIsolatedExecutor`] runs each check on its own task (a panic becomes a
failure); [`InlineExecutor`] runs in place. Selection, manifest gating, and
report assembly stay in [`run`]/[`run_with`].

## Running

```sh
# Spawn a devnet derived from the manifest, run the suite, tear it down
cargo run -p xtask -- acceptance --manifest manifests/devnet.toml --report report.json

# Run against the deployed alphanet (Cloudflare Access via env)
CF_ACCESS_CLIENT_ID=... CF_ACCESS_CLIENT_SECRET=... \
cargo run -p xtask -- acceptance \
  --manifest manifests/alphanet.toml \
  --target alphanet \
  --rpc-url https://<alphanet-rpc> \
  --l1-rpc-url https://<l1-rpc> \
  --category health,spec \
  --strict \
  --report report.json
```

The command prints a summary, optionally writes JSON/Markdown reports, and exits
non-zero if any executed check fails (or, under `--strict`, if any committed
requirement is uncovered).
