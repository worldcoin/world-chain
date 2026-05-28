//! reth ExEx entrypoint for the World Chain Relayer (cacher role).
//!
//! Implements the cacher's notification loop. The body:
//!
//! * opens the [`WithdrawalStore`] at the relayer datadir,
//! * on `ChainCommitted { new }` and the `new` side of `ChainReorged` scans the
//!   committed chain for `MessagePassed` withdrawals and caches them,
//! * on `ChainReverted { old }` and the `old` side of `ChainReorged` prunes the
//!   reverted block range (reorg rule),
//! * persists the cache head from the committed tip **before** forwarding
//!   `ExExEvent::FinishedHeight`, so a height is never acknowledged until its
//!   withdrawals are durably persisted.
//!
//! When disabled it drains the notification stream and forwards
//! `FinishedHeight` so it never stalls pruning — mirroring the proposer's
//! `drain_until_closed`.
//!
//! The relayer **driver** (proof construction, L1 prove/finalize, dispute-game
//! polling) is out of scope here; this module only runs the cacher.

use std::{path::PathBuf, sync::Arc};

use alloy_consensus::TxReceipt;
use alloy_primitives::Log;
use futures::TryStreamExt;
use reth_exex::{ExExContext, ExExEvent};
use reth_node_api::{FullNodeComponents, NodePrimitives, NodeTypes};
use tracing::{info, warn};

use crate::{
    Result,
    error::OpProposerError,
    proposer::db::StoredHead,
    withdrawals::{
        cacher::{prune_chain, scan_chain},
        config::{RelayerCliArgs, RelayerConfig},
        store::WithdrawalStore,
    },
};

const TARGET: &str = "exex::relayer";

/// Trait bound capturing the node primitives the cacher needs: receipts whose
/// logs are `alloy_primitives::Log`.
///
/// The blanket impl means any node whose primitives already satisfy this is
/// automatically usable.
pub trait CacherPrimitives: NodePrimitives<Receipt: TxReceipt<Log = Log>> {}

impl<N> CacherPrimitives for N where N: NodePrimitives<Receipt: TxReceipt<Log = Log>> {}

/// Spawned reth ExEx future for the World Chain Relayer cacher.
pub async fn world_chain_relayer_exex<N>(mut ctx: ExExContext<N>, cfg: RelayerConfig) -> Result<()>
where
    N: FullNodeComponents,
    <N::Types as NodeTypes>::Primitives: CacherPrimitives,
{
    let store =
        Arc::new(WithdrawalStore::open(&cfg.datadir).map_err(OpProposerError::WithdrawalStore)?);
    info!(
        target: TARGET,
        datadir = ?store.path(),
        cache_only = cfg.cache_only,
        head = ?ctx.head,
        "World Chain Relayer cacher running",
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

        // Reverted side first: prune before re-caching the new side on a
        // reorg so a withdrawal re-emitted at a new block is not transiently
        // dropped.
        if let Some(reverted) = notification.reverted_chain()
            && let Err(e) = prune_chain(reverted.as_ref(), &store)
        {
            warn!(target: TARGET, error = ?e, "failed to prune reverted withdrawals");
        }

        if let Some(committed) = notification.committed_chain() {
            match scan_chain(committed.as_ref(), &store) {
                Ok(stats) if stats.rejected > 0 => {
                    warn!(
                        target: TARGET,
                        rejected = stats.rejected,
                        cached = stats.cached,
                        "scanned committed chain; some MessagePassed logs were rejected",
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    warn!(target: TARGET, error = ?e, "failed to scan committed chain");
                }
            }

            // Persist the head durably *before* acknowledging the height, so a
            // crash can never lose a withdrawal whose height we already
            // acknowledged.
            let num_hash = committed.tip().num_hash();
            if let Err(e) = store.put_head(StoredHead {
                block_number: num_hash.number,
                block_hash: num_hash.hash,
            }) {
                warn!(
                    target: TARGET,
                    error = ?e,
                    block = num_hash.number,
                    "failed to persist cacher head",
                );
            }
            ctx.events
                .send(ExExEvent::FinishedHeight(num_hash))
                .map_err(|e| OpProposerError::msg(format!("exex events channel: {e}")))?;
        }
    }

    info!(target: TARGET, "ExEx notifications channel closed, shutting down");
    Ok(())
}

/// Wraps the relayer ExEx with an enable flag.
pub async fn install_world_chain_relayer_exex<N>(
    ctx: ExExContext<N>,
    args: RelayerCliArgs,
    fallback_datadir: PathBuf,
) -> Result<()>
where
    N: FullNodeComponents,
    <N::Types as NodeTypes>::Primitives: CacherPrimitives,
{
    if !args.enabled {
        info!(
            target: TARGET,
            "World Chain Relayer disabled; ExEx will only drain notifications",
        );
        return drain_until_closed(ctx).await;
    }
    let cfg = args.into_config(fallback_datadir)?;
    world_chain_relayer_exex(ctx, cfg).await
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
