//! reth ExEx entrypoint for the OP Proposer.
//!
//! The proposer logic runs in a self-contained task spawned at install time.
//! The ExEx body keeps the channel alive, persists the latest committed head
//! into the proposer MDBX store, and forwards `ExExEvent::FinishedHeight` so
//! reth knows it can prune up to that block.

use std::{path::PathBuf, sync::Arc};

use futures::TryStreamExt;
use reth_exex::{ExExContext, ExExEvent, ExExNotification};
use reth_node_api::FullNodeComponents;
use tracing::{debug, info, warn};

use crate::{
    Result,
    error::OpProposerError,
    proposer::{
        config::{ProposerCliArgs, ProposerConfig},
        db::StoredHead,
        local_node::{ProviderBounds, local_reader_from_ctx},
        service::{AdminRpcSettings, ProposerService},
        source::{ProposalSource, local::LocalProposalSource},
    },
};

/// Spawned reth ExEx future for the OP Proposer.
pub async fn op_proposer_exex<N>(mut ctx: ExExContext<N>, cfg: ProposerConfig) -> Result<()>
where
    N: FullNodeComponents,
    N::Provider: ProviderBounds,
{
    let admin_settings = AdminRpcSettings::from_config(&cfg);

    let reader = local_reader_from_ctx::<N>(&ctx);
    let source: Arc<dyn ProposalSource> = Arc::new(LocalProposalSource::new(reader));
    info!(
        target: "exex::proposer",
        "using local proposal source (state read directly from node)",
    );

    let mut service = ProposerService::from_config_with_source(cfg, source).await?;
    service.start(&admin_settings).await?;

    info!(
        target: "exex::proposer",
        head = ?ctx.head,
        "OP Proposer ExEx running",
    );

    loop {
        let notification = match ctx
            .notifications
            .try_next()
            .await
            .map_err(|e| OpProposerError::msg(format!("exex notifications channel: {e}")))?
        {
            Some(n) => n,
            None => break,
        };
        match &notification {
            ExExNotification::ChainCommitted { new } => {
                debug!(target: "exex::proposer", range = ?new.range(), "chain committed");
            }
            ExExNotification::ChainReorged { old, new } => {
                debug!(
                    target: "exex::proposer",
                    from = ?old.range(),
                    to = ?new.range(),
                    "chain reorged",
                );
            }
            ExExNotification::ChainReverted { old } => {
                debug!(target: "exex::proposer", range = ?old.range(), "chain reverted");
            }
        }

        if let Some(committed) = notification.committed_chain() {
            let num_hash = committed.tip().num_hash();
            if let Err(e) = service.store.put_head(StoredHead {
                block_number: num_hash.number,
                block_hash: num_hash.hash,
            }) {
                warn!(
                    target: "exex::proposer",
                    error = ?e,
                    block = num_hash.number,
                    "failed to persist proposer head",
                );
            }
            ctx.events
                .send(ExExEvent::FinishedHeight(num_hash))
                .map_err(|e| OpProposerError::msg(format!("exex events channel: {e}")))?;
        }
    }

    info!(target: "exex::proposer", "ExEx notifications channel closed, shutting down");
    let _ = service.stop().await;
    Ok(())
}

/// Wraps the proposer ExEx with an enable flag.
pub async fn install_op_proposer_exex<N>(
    ctx: ExExContext<N>,
    args: ProposerCliArgs,
    fallback_datadir: PathBuf,
) -> Result<()>
where
    N: FullNodeComponents,
    N::Provider: ProviderBounds,
{
    if !args.enabled {
        info!(target: "exex::proposer", "OP Proposer disabled; ExEx will only drain notifications");
        return drain_until_closed(ctx).await;
    }
    let cfg = args.into_config(fallback_datadir)?;
    op_proposer_exex(ctx, cfg).await
}

async fn drain_until_closed<N: FullNodeComponents>(mut ctx: ExExContext<N>) -> Result<()> {
    while let Some(notification) = ctx
        .notifications
        .try_next()
        .await
        .map_err(|e| OpProposerError::msg(format!("exex notifications channel: {e}")))?
    {
        if let Some(committed) = notification.committed_chain() {
            ctx.events
                .send(ExExEvent::FinishedHeight(committed.tip().num_hash()))
                .map_err(|e| OpProposerError::msg(format!("exex events channel: {e}")))?;
        }
    }
    Ok(())
}
