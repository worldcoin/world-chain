//! Acceptance checks for the Jovian hardfork.
//!
//! Run when the manifest's committed hardfork is at or after `jovian`.

use std::sync::Arc;

use eyre::eyre::bail;

use crate::{TestCtx, acceptance_test};

/// Blocks carry the post-Ecotone (EIP-4844) header fields a Jovian-active chain
/// must produce.
#[acceptance_test(category = SpecCompatibility, requires(hardfork = jovian))]
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
