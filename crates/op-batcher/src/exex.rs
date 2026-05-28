//! reth ExEx entrypoint for the OP Batcher.
//!
//! The batcher logic runs in a self-contained task spawned at install time
//! (`BatchSubmitter`). The ExEx body keeps the notifications channel alive,
//! persists the latest committed L2 head into the batcher MDBX store, and
//! forwards `ExExEvent::FinishedHeight` so reth can prune. Mirrors the proposer
//! crate's `exex` module.

use std::{path::PathBuf, sync::Arc};

use futures::TryStreamExt;
use reth_exex::{ExExContext, ExExEvent, ExExNotification};
use reth_node_api::{FullNodeComponents, NodeTypes};
use reth_optimism_primitives::OpPrimitives;
use tracing::{debug, info, warn};

use crate::{
    Result,
    config::{BatcherCliArgs, BatcherConfig},
    db::StoredHead,
    error::OpBatcherError,
    local_node::{ProviderBounds, local_source_from_ctx},
    service::{AdminRpcSettings, BatcherService},
    source::LocalBlockSource,
};

/// Spawned reth ExEx future for the OP Batcher.
pub async fn op_batcher_exex<N>(mut ctx: ExExContext<N>, cfg: BatcherConfig) -> Result<()>
where
    N: FullNodeComponents<Types: NodeTypes<Primitives = OpPrimitives>>,
    N::Provider: ProviderBounds,
{
    let admin_settings = AdminRpcSettings::from_config(&cfg);

    let source: Arc<dyn LocalBlockSource> = local_source_from_ctx::<N>(&ctx);
    info!(
        target: "exex::batcher",
        "using local block source (L2 read directly from node state)",
    );

    let mut service = BatcherService::from_config_with_source(cfg, source).await?;
    service.start(&admin_settings).await?;

    info!(target: "exex::batcher", head = ?ctx.head, "OP Batcher ExEx running");

    loop {
        let notification = match ctx
            .notifications
            .try_next()
            .await
            .map_err(|e| OpBatcherError::msg(format!("exex notifications channel: {e}")))?
        {
            Some(n) => n,
            None => break,
        };
        match &notification {
            ExExNotification::ChainCommitted { new } => {
                debug!(target: "exex::batcher", range = ?new.range(), "chain committed");
            }
            ExExNotification::ChainReorged { old, new } => {
                debug!(target: "exex::batcher", from = ?old.range(), to = ?new.range(), "chain reorged");
            }
            ExExNotification::ChainReverted { old } => {
                debug!(target: "exex::batcher", range = ?old.range(), "chain reverted");
            }
        }

        if let Some(committed) = notification.committed_chain() {
            let num_hash = committed.tip().num_hash();
            if let Err(e) = service.store.put_head(StoredHead {
                block_number: num_hash.number,
                block_hash: num_hash.hash,
            }) {
                warn!(
                    target: "exex::batcher",
                    error = ?e,
                    block = num_hash.number,
                    "failed to persist batcher head",
                );
            }
            ctx.events
                .send(ExExEvent::FinishedHeight(num_hash))
                .map_err(|e| OpBatcherError::msg(format!("exex events channel: {e}")))?;
        }
    }

    info!(target: "exex::batcher", "ExEx notifications channel closed, shutting down");
    let _ = service.stop().await;
    Ok(())
}

/// Wraps the batcher ExEx with an enable flag.
pub async fn install_op_batcher_exex<N>(
    ctx: ExExContext<N>,
    args: BatcherCliArgs,
    fallback_datadir: PathBuf,
) -> Result<()>
where
    N: FullNodeComponents<Types: NodeTypes<Primitives = OpPrimitives>>,
    N::Provider: ProviderBounds,
{
    if !args.enabled {
        info!(target: "exex::batcher", "OP Batcher disabled; ExEx will only drain notifications");
        return drain_until_closed(ctx).await;
    }
    let cfg = args.into_config(fallback_datadir)?;
    op_batcher_exex(ctx, cfg).await
}

async fn drain_until_closed<N: FullNodeComponents>(mut ctx: ExExContext<N>) -> Result<()> {
    while let Some(notification) = ctx
        .notifications
        .try_next()
        .await
        .map_err(|e| OpBatcherError::msg(format!("exex notifications channel: {e}")))?
    {
        if let Some(committed) = notification.committed_chain() {
            ctx.events
                .send(ExExEvent::FinishedHeight(committed.tip().num_hash()))
                .map_err(|e| OpBatcherError::msg(format!("exex events channel: {e}")))?;
        }
    }
    Ok(())
}
