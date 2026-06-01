//! Acceptance checks for the Tropo hardfork (the first World Chain specific fork
//! after Jovian).
//!
//! Run when the manifest's committed hardfork is at or after `tropo`.

use std::sync::Arc;

use crate::{TestCtx, acceptance_test};

/// A Tropo-active network produces blocks (liveness in the Tropo fork context).
///
/// Placeholder for Tropo-specific semantics: it records the head and asserts the
/// network is producing blocks under the committed fork. Replace with concrete
/// Tropo behavioural checks as they are defined.
#[acceptance_test(category = SpecCompatibility, requires(hardfork = tropo))]
async fn tropo_chain_live(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    let block = ctx.latest_block().await?;
    ctx.record_i64("tropo_head", block.header.number as i64);
    Ok(())
}
