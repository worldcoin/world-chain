use clap::Parser;
use eyre::config::HookBuilder;
use reth_node_builder::NodeHandle;
use reth_optimism_cli::{Cli, chainspec::OpChainSpecParser};
use reth_payload_builder::PayloadStore;
use reth_tracing::tracing::info;
use world_chain_kona::{InProcessEngineClient, KonaConfig, KonaServiceHandle};
use world_chain_node::{
    FlashblocksOpApi, OpApiExtServer, args::WorldChainArgs, config::WorldChainNodeConfig,
    context::FlashblocksContext, node::WorldChainNode,
};
use world_chain_rpc::{EthApiExtServer, SequencerClient, WorldChainEthApiExt};

#[cfg(all(feature = "jemalloc", unix))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() {
    dotenvy::dotenv().ok();

    reth_cli_util::sigsegv_handler::install();

    HookBuilder::default()
        .theme(eyre::config::Theme::new())
        .install()
        .expect("failed to install error handler");

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly provided.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        unsafe {
            std::env::set_var("RUST_BACKTRACE", "1");
        }
    }

    // Set default log level
    if std::env::var_os("RUST_LOG").is_none() {
        unsafe {
            std::env::set_var("RUST_LOG", "info,reth=info");
        }
    }

    if let Err(err) =
        Cli::<OpChainSpecParser, WorldChainArgs>::parse().run(|mut builder, args| async move {
            info!(target: "reth::cli", "Launching node");

            let kona_args = args.kona.clone();
            let config: WorldChainNodeConfig = args.into_config(builder.config_mut())?;

            info!(target: "reth::cli", "Starting in Flashblocks mode");
            let node = WorldChainNode::<FlashblocksContext>::new(config.clone());
            let NodeHandle {
                node_exit_future,
                node: full_node,
            } = builder
                .node(node)
                .extend_rpc_modules(move |ctx| {
                    let provider = ctx.provider().clone();
                    let pool = ctx.pool().clone();
                    let sequencer_client = config.args.rollup.sequencer.map(SequencerClient::new);
                    let eth_api_ext = WorldChainEthApiExt::new(pool, provider, sequencer_client);
                    ctx.modules.replace_configured(eth_api_ext.into_rpc())?;
                    ctx.modules
                        .replace_configured(FlashblocksOpApi.into_rpc())?;
                    Ok(())
                })
                .launch()
                .await?;

            let kona_enabled = kona_args.as_ref().is_some_and(|k| k.enabled);
            if kona_enabled {
                let kona_args = kona_args.expect("already checked");

                let l1_rpc_url: url::Url = kona_args.l1_rpc_url.parse()?;
                let l1_beacon_url: url::Url = kona_args.l1_beacon_url.parse()?;

                let rollup_config = if let Some(path) = &kona_args.rollup_config_path {
                    let config_json = std::fs::read_to_string(path)
                        .map_err(|e| eyre::Report::msg(format!(
                            "failed to read rollup config from {}: {e}", path.display()
                        )))?;
                    let config: kona_genesis::RollupConfig = serde_json::from_str(&config_json)
                        .map_err(|e| eyre::Report::msg(format!(
                            "failed to parse rollup config: {e}"
                        )))?;
                    std::sync::Arc::new(config)
                } else {
                    return Err(eyre::Report::msg(
                        "--kona.rollup-config is required when --kona.enabled is set"
                    ));
                };

                let kona_config = KonaConfig {
                    rollup_config: rollup_config.clone(),
                    l1_rpc_url: l1_rpc_url.clone(),
                    l1_beacon_url,
                    l1_trust_rpc: kona_args.l1_trust_rpc,
                    l2_trust_rpc: false,
                    sequencer_mode: false,
                    p2p: kona_args.p2p,
                    rpc_listen_addr: None,
                    l1_slot_duration_override: None,
                };

                let engine_handle = full_node.consensus_engine_handle().clone();
                let l2_provider = full_node.provider.clone();
                let payload_store =
                    PayloadStore::new(full_node.payload_builder_handle.clone());
                let l1_provider =
                    alloy_provider::RootProvider::new_http(l1_rpc_url);

                // L2 RPC provider for block reads / proofs. Points at reth's own HTTP endpoint
                // so we get proper RPC type conversion for free.
                // TODO(kona-integration): Extract actual HTTP port from reth's launched config.
                let l2_rpc_url: url::Url = "http://127.0.0.1:8545".parse()?;
                let l2_rpc =
                    alloy_provider::RootProvider::<op_alloy_network::Optimism>::new_http(l2_rpc_url);

                let engine_client = InProcessEngineClient::new(
                    rollup_config,
                    engine_handle,
                    l2_provider,
                    payload_store,
                    l1_provider,
                    l2_rpc,
                );

                // Reth's auth RPC URL and JWT for the Kona derivation pipeline.
                // TODO(kona-integration): Extract the actual auth RPC port and JWT secret from
                // reth's launched config rather than using defaults.
                let l2_auth_rpc_url: url::Url = "http://127.0.0.1:8551".parse()?;
                let jwt_secret = alloy_rpc_types_engine::JwtSecret::random();

                let mut kona_handle =
                    KonaServiceHandle::spawn(kona_config, engine_client, l2_auth_rpc_url, jwt_secret).await?;

                info!(target: "reth::cli", "Kona consensus node started in-process");

                tokio::select! {
                    result = node_exit_future => {
                        info!(target: "reth::cli", "Reth exited");
                        kona_handle.shutdown();
                        result?;
                    }
                    result = kona_handle.stopped() => {
                        info!(target: "reth::cli", "Kona exited");
                        if let Err(e) = result {
                            return Err(eyre::Report::msg(e));
                        }
                    }
                }
            } else {
                node_exit_future.await?;
            }

            Ok(())
        })
    {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}
