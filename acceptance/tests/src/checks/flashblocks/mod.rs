//! Acceptance checks for the flashblocks feature.
//!
//! Run only when the manifest commits to `feature = flashblocks`.

use std::sync::Arc;

use eyre::eyre::bail;

use crate::{TestCtx, acceptance_test};

/// Capability string World Chain advertises for flashblocks.
const FLASHBLOCKS_CAPABILITY: &str = "flashblocksv1";

/// `op_supportedCapabilities` advertises flashblocks support.
#[acceptance_test(category = SpecCompatibility, requires(feature = flashblocks))]
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
