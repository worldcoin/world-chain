//! World Chain node launcher with proof-history support.

use crate::{context::WorldChainDefaultContext, node::WorldChainNode};
use eyre::eyre::eyre;
use futures_util::FutureExt;
use reth_db::DatabaseEnv;
use reth_db_api::database_metrics::DatabaseMetrics;
use reth_node_builder::{FullNodeComponents, NodeBuilder, WithLaunchContext};
use reth_optimism_exex::OpProofsExEx;
use reth_optimism_node::args::ProofsStorageVersion;
use reth_optimism_rpc::{
    debug::{DebugApiExt, DebugApiOverrideServer},
    eth::proofs::{EthApiExt, EthApiOverrideServer},
};
use reth_optimism_trie::{
    OpProofsStorage, OpProofsStore,
    db::{MdbxProofsStorage, MdbxProofsStorageV2},
};
use reth_tasks::TaskExecutor;
use std::{sync::Arc, time::Duration};
use tokio::time::sleep;
use tracing::info;
use world_chain_chainspec::WorldChainSpec;
use world_chain_cli::WorldChainNodeConfig;

/// Launches a World Chain node, enabling proof history when requested by
/// `--proofs-history`.
pub async fn launch_node(
    builder: WithLaunchContext<NodeBuilder<DatabaseEnv, WorldChainSpec>>,
    config: WorldChainNodeConfig,
) -> eyre::Result<()> {
    if !config.args.rollup.proofs_history {
        let handle = builder
            .node(WorldChainNode::<WorldChainDefaultContext>::new(config))
            .launch()
            .await?;
        return handle.node_exit_future.await;
    }

    // Defaults to `<reth-data-dir>/historical-proofs` when no explicit storage path is supplied.
    let path = config
        .args
        .rollup
        .history
        .resolve_storage_path(builder.config().datadir().as_ref());

    match config.args.rollup.history.storage_version {
        ProofsStorageVersion::V1 => {
            info!(target: "reth::cli", "Using on-disk storage for proofs history (v1)");
            let storage = Arc::new(
                MdbxProofsStorage::new(&path)
                    .map_err(|err| eyre!("failed to open proofs-history storage v1: {err}"))?,
            );
            launch_with_proof_history(builder, config, storage).await
        }
        ProofsStorageVersion::V2 => {
            info!(target: "reth::cli", "Using on-disk storage for proofs history (v2)");
            let storage = Arc::new(
                MdbxProofsStorageV2::new(&path)
                    .map_err(|err| eyre!("failed to open proofs-history storage v2: {err}"))?,
            );
            launch_with_proof_history(builder, config, storage).await
        }
    }
}

/// Installs the proof-history ExEx, RPC overrides, and storage metrics before launching the node.
pub async fn launch_with_proof_history<S>(
    builder: WithLaunchContext<NodeBuilder<DatabaseEnv, WorldChainSpec>>,
    config: WorldChainNodeConfig,
    storage: Arc<S>,
) -> eyre::Result<()>
where
    S: OpProofsStore + DatabaseMetrics + Send + Sync + 'static,
{
    let proofs_storage: OpProofsStorage<Arc<S>> = storage.clone().into();
    let exex_storage = proofs_storage.clone();

    let proofs_history_window = config.args.rollup.proofs_history_window.window;
    let proofs_history_verification_interval =
        config.args.rollup.proofs_history_verification_interval;

    let handle = builder
        .node(WorldChainNode::<WorldChainDefaultContext>::new(config))
        .on_node_started(move |node| {
            spawn_proofs_db_metrics(
                node.task_executor,
                storage,
                node.config.metrics.push_gateway_interval,
            );
            Ok(())
        })
        .install_exex("proofs-history", async move |exex_context| {
            Ok(OpProofsExEx::builder(exex_context, exex_storage)
                .with_proofs_history_window(proofs_history_window)
                .with_verification_interval(proofs_history_verification_interval)
                .build()
                .run()
                .boxed())
        })
        .extend_rpc_modules(move |ctx| {
            info!(target: "reth::cli", "Installing proofs-history RPC overrides (eth_getProof, debug_executePayload)");

            let eth_api = EthApiExt::new(ctx.registry.eth_api().clone(), proofs_storage.clone());
            let auth_eth_api =
                EthApiExt::new(ctx.registry.eth_api().clone(), proofs_storage.clone());
            let debug_api = DebugApiExt::new(
                ctx.node().provider().clone(),
                ctx.registry.eth_api().clone(),
                proofs_storage,
                ctx.node().task_executor().clone(),
                ctx.node().evm_config().clone(),
            );

            let eth_replaced = ctx.modules.replace_configured(eth_api.into_rpc())?;
            let auth_eth_replaced = ctx
                .auth_module
                .replace_auth_methods(auth_eth_api.into_rpc())?;
            let debug_replaced = ctx.modules.replace_configured(debug_api.into_rpc())?;
            info!(target: "reth::cli", eth_replaced, auth_eth_replaced, debug_replaced, "Proofs-history RPC overrides installed");
            Ok(())
        })
        .launch_with_debug_capabilities()
        .await?;

    handle.node_exit_future.await
}

/// Spawns a task that periodically reports metrics for the proofs database.
fn spawn_proofs_db_metrics<S>(
    executor: TaskExecutor,
    storage: Arc<S>,
    metrics_report_interval: Duration,
) where
    S: DatabaseMetrics + Send + Sync + 'static,
{
    executor.spawn_critical_task("op-proofs-storage-metrics", async move {
        info!(
            target: "reth::cli",
            ?metrics_report_interval,
            "Starting op-proofs-storage metrics task"
        );

        loop {
            sleep(metrics_report_interval).await;
            storage.report_metrics();
        }
    });
}
