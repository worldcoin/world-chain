//! Acceptance checks for the Karst hardfork.
//!
//! Run when the manifest's committed hardfork is at or after `karst`.

use std::sync::Arc;

use crate::{TestCtx, acceptance_test};

/// A Karst-active network produces blocks (liveness in the Karst fork context).
///
/// Placeholder for Karst-specific semantics: it records the head and asserts the
/// network is producing blocks under the committed fork. Replace with concrete
/// Karst behavioural checks as they are defined.
#[acceptance_test(category = SpecCompatibility, requires(hardfork = karst))]
async fn karst_chain_live(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    let block = ctx.latest_block().await?;
    ctx.record_i64("karst_head", block.header.number as i64);
    Ok(())
}
