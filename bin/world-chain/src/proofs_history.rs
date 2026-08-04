//! Node launcher with op-reth proofs-history support.
//!
//! Mirrors `reth_optimism_node::proof_history::launch_node`, which cannot be reused directly: it is
//! hardcoded to `OpNode`/`OpChainSpec` while this binary launches [`WorldChainNode`].

use eyre::eyre::eyre;
use futures_util::FutureExt;
use reth_db::{DatabaseEnv, database_metrics::DatabaseMetrics};
use reth_node_builder::{FullNodeComponents, NodeBuilder, NodeHandle, WithLaunchContext};
use reth_optimism_exex::OpProofsExEx;
use reth_optimism_node::args::ProofsStorageVersion;
use reth_optimism_payload_builder::OpPayloadBuilderAttributes;
use reth_optimism_primitives::OpTransactionSigned;
use reth_optimism_rpc::{
    debug::{DebugApiExt, DebugApiOverrideServer},
    eth::proofs::{EthApiExt, EthApiOverrideServer},
};
use reth_optimism_trie::{
    OpProofsStorage, OpProofsStore,
    db::{MdbxProofsStorage, MdbxProofsStorageV2},
};
use reth_tasks::TaskExecutor;
use reth_tracing::tracing::info;
use std::{sync::Arc, time::Duration};
use world_chain_chainspec::WorldChainSpec;
use world_chain_cli::WorldChainNodeConfig;
use world_chain_node::{context::WorldChainDefaultContext, node::WorldChainNode};

/// Payload attributes for the `debug_executePayload` override; must match `WorldChainAddOns`, whose
/// `OpDebugWitnessApi` registration of that method this override replaces.
type Attributes = OpPayloadBuilderAttributes<OpTransactionSigned>;

/// Launches the World Chain node, installing the proofs-history ExEx and RPC overrides when
/// `--proofs-history` is set.
pub async fn launch_node(
    builder: WithLaunchContext<NodeBuilder<DatabaseEnv, WorldChainSpec>>,
    config: WorldChainNodeConfig,
) -> eyre::Result<()> {
    if !config.args.rollup.proofs_history {
        let node = WorldChainNode::<WorldChainDefaultContext>::new(config);
        let NodeHandle {
            node_exit_future,
            node: _node,
        } = builder.node(node).launch().await?;
        return node_exit_future.await;
    }

    // Defaults to `<reth-data-dir>/historical-proofs` when not supplied.
    let history = config.args.rollup.history.clone();
    let path = history.resolve_storage_path(builder.config().datadir().as_ref());

    match history.storage_version {
        ProofsStorageVersion::V1 => {
            info!(target: "reth::cli", ?path, "Opening proofs-history storage (v1)");
            let storage = Arc::new(
                MdbxProofsStorage::new(&path)
                    .map_err(|e| eyre!("failed to open proofs-history storage v1: {e}"))?,
            );
            launch_with_proofs_history(builder, config, storage).await
        }
        ProofsStorageVersion::V2 => {
            info!(target: "reth::cli", ?path, "Opening proofs-history storage (v2)");
            let storage = Arc::new(
                MdbxProofsStorageV2::new(&path)
                    .map_err(|e| eyre!("failed to open proofs-history storage v2: {e}"))?,
            );
            launch_with_proofs_history(builder, config, storage).await
        }
    }
}

/// Installs the ExEx, RPC overrides, and DB metrics hook, then launches the node.
async fn launch_with_proofs_history<S>(
    builder: WithLaunchContext<NodeBuilder<DatabaseEnv, WorldChainSpec>>,
    config: WorldChainNodeConfig,
    mdbx: Arc<S>,
) -> eyre::Result<()>
where
    S: OpProofsStore + DatabaseMetrics + Send + Sync + 'static,
{
    let storage: OpProofsStorage<Arc<S>> = mdbx.clone().into();
    let storage_exex = storage.clone();

    let window = config.args.rollup.proofs_history_window.window;
    let verification_interval = config.args.rollup.proofs_history_verification_interval;
    let node = WorldChainNode::<WorldChainDefaultContext>::new(config);

    let NodeHandle {
        node_exit_future,
        node: _node,
    } = builder
        .node(node)
        .on_node_started(move |node| {
            spawn_proofs_db_metrics(
                node.task_executor,
                mdbx,
                node.config.metrics.push_gateway_interval,
            );
            Ok(())
        })
        .install_exex("proofs-history", async move |exex_context| {
            Ok(OpProofsExEx::builder(exex_context, storage_exex)
                .with_proofs_history_window(window)
                .with_verification_interval(verification_interval)
                .build()
                .run()
                .boxed())
        })
        .extend_rpc_modules(move |ctx| {
            let eth_ext = EthApiExt::new(ctx.registry.eth_api().clone(), storage.clone());
            let auth_eth_ext = EthApiExt::new(ctx.registry.eth_api().clone(), storage.clone());
            let debug_ext = DebugApiExt::<_, _, _, _, Attributes>::new(
                ctx.node().provider().clone(),
                ctx.registry.eth_api().clone(),
                storage,
                ctx.node().task_executor().clone(),
                ctx.node().evm_config().clone(),
            );

            let eth_replaced = ctx.modules.replace_configured(eth_ext.into_rpc())?;
            let auth_eth_replaced = ctx
                .auth_module
                .replace_auth_methods(auth_eth_ext.into_rpc())?;
            let debug_replaced = ctx.modules.replace_configured(debug_ext.into_rpc())?;

            info!(
                target: "reth::cli",
                eth_replaced,
                auth_eth_replaced,
                debug_replaced,
                "Installed proofs-history RPC overrides"
            );
            Ok(())
        })
        .launch()
        .await?;

    node_exit_future.await
}

/// Spawns a task that periodically reports MDBX gauges for the proofs-history DB.
fn spawn_proofs_db_metrics<S>(executor: TaskExecutor, storage: Arc<S>, report_interval: Duration)
where
    S: DatabaseMetrics + Send + Sync + 'static,
{
    executor.spawn_critical_task("proofs-history-storage-metrics", async move {
        loop {
            tokio::time::sleep(report_interval).await;
            storage.report_metrics();
        }
    });
}
