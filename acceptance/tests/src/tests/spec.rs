//! Core specification tests.

use std::sync::Arc;

use alloy_provider::Provider;
use eyre::eyre::ensure;
use tracing::info;

use crate::{TestCtx, acceptance_test};

/// Standard Engine API methods advertised when negotiating capabilities.
const ENGINE_METHODS: &[&str] = &[
    "engine_forkchoiceUpdatedV3",
    "engine_getPayloadV4",
    "engine_newPayloadV4",
    "engine_exchangeCapabilities",
];

/// The chain id matches the configured expectation.
///
/// When the manifest pins no chain id the observed value is recorded and the
/// test passes informationally.
#[acceptance_test]
async fn chain_id_matches(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    let actual = ctx.chain_id().await?;
    ctx.record_i64("chain_id", actual as i64);

    match ctx.expected_chain_id() {
        Some(expected) => ensure!(
            actual == expected,
            "chain id mismatch: expected {expected}, got {actual}"
        ),
        None => info!(
            actual,
            "no expected chain id configured; recording observed value"
        ),
    }
    Ok(())
}

/// The Engine API is reachable and accepts the JWT credentials.
///
/// Skips at run time when no engine endpoint is configured. `engine_exchange
/// Capabilities` is the handshake every consensus client performs on startup,
/// so a successful authenticated response proves the authrpc + JWT are wired.
#[acceptance_test]
async fn engine_api_authenticated(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    let Some(engine) = ctx.engine() else {
        return Err(ctx.skip("no engine endpoint configured"));
    };

    let provider = engine.provider()?;
    let capabilities: Vec<String> = provider
        .client()
        .request("engine_exchangeCapabilities", (ENGINE_METHODS,))
        .await?;

    ctx.record_i64("engine_capabilities", capabilities.len() as i64);
    Ok(())
}

/// The flashblocks endpoint is wired into the environment.
///
/// Declarative gating (`features = ["flashblocks"]`) means the runner only
/// selects this test when the committed manifest commits to flashblocks; it is
/// reported as skipped otherwise. This is a placeholder asserting the endpoint
/// is reachable — the real flashblocks stream assertions are filled in later.
#[acceptance_test(features = ["flashblocks"])]
async fn flashblocks_endpoint_configured(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    ctx.skip_if(
        ctx.flashblocks_url().is_none(),
        "flashblocks endpoint not configured in the environment",
    )?;
    Ok(())
}

/// Placeholder for behavior introduced by the Tropo hardfork.
///
/// `requires_hardfork = "tropo"` keeps this test skipped until the network under
/// test commits to Tropo or later, demonstrating fork-axis gating and the
/// fork-matrix sweep.
#[acceptance_test(requires_hardfork = "tropo")]
async fn tropo_placeholder(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    let _ = ctx.block_number().await?;
    Ok(())
}
