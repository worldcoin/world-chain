//! Acceptance checks for the block access list (BAL) feature.
//!
//! Run only when the manifest commits to `feature = block_access_list`. BAL is
//! produced through the flashblocks pipeline, so the network must also advertise
//! the flashblocks capability.

use std::sync::Arc;

use eyre::eyre::bail;

use crate::{TestCtx, acceptance_test};

const FLASHBLOCKS_CAPABILITY: &str = "flashblocksv1";

/// A BAL-producing network advertises the flashblocks capability that carries
/// it.
///
/// This is the minimal structural commitment check; deeper verification (that
/// produced blocks actually carry a well-formed access list) is a follow-up
/// once the RPC surface for inspecting BALs is wired.
#[acceptance_test(category = SpecCompatibility, requires(feature = block_access_list))]
async fn block_access_list_pipeline_available(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    let capabilities = ctx.supported_capabilities().await?;
    if !capabilities.iter().any(|cap| cap == FLASHBLOCKS_CAPABILITY) {
        bail!(
            "block access lists require the flashblocks pipeline, but `{FLASHBLOCKS_CAPABILITY}` is not advertised: {capabilities:?}"
        );
    }
    Ok(())
}
