//! Specification-compatibility checks: the deployment matches the World Chain
//! spec (identity and advertised capabilities).

use std::sync::Arc;

use eyre::eyre::{bail, ensure};
use tracing::info;

use crate::{TestCtx, acceptance_test};

/// World Chain advertises the flashblocks capability.
const FLASHBLOCKS_CAPABILITY: &str = "flashblocksv1";

/// The chain id matches the configured expectation.
///
/// When no expected chain id is configured the observed value is recorded and
/// the check passes informationally.
#[acceptance_test(category = SpecCompatibility)]
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

/// `op_supportedCapabilities` advertises flashblocks support.
///
/// Runs only when the manifest commits to the `flashblocks` feature.
#[acceptance_test(category = SpecCompatibility, requires(flashblocks))]
async fn supports_flashblocks_capability(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    let capabilities = ctx.supported_capabilities().await?;
    ctx.record_text("capabilities", capabilities.join(","));

    if !capabilities.iter().any(|cap| cap == FLASHBLOCKS_CAPABILITY) {
        bail!(
            "expected `{FLASHBLOCKS_CAPABILITY}` in op_supportedCapabilities, got {capabilities:?}"
        );
    }
    Ok(())
}

/// Blocks carry the post-Ecotone (4844) header fields that a Jovian-active
/// chain must produce.
///
/// Runs only when the manifest commits to the `jovian` hardfork. This is a
/// concrete example of a fork-gated commitment check; deeper per-fork semantic
/// verification can be added the same way.
#[acceptance_test(category = SpecCompatibility, requires(jovian))]
async fn jovian_block_header_format(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    let block = ctx.latest_block().await?;

    if block.header.blob_gas_used.is_none() {
        bail!(
            "latest block {} is missing the post-Ecotone `blobGasUsed` header field expected under Jovian",
            block.header.number
        );
    }
    Ok(())
}
