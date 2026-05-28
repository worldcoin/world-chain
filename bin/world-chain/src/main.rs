use clap::Parser;
use eyre::config::HookBuilder;
use reth_node_builder::NodeHandle;
use reth_optimism_consensus::OpBeaconConsensus;
use reth_payload_builder::PayloadStore;
use reth_tracing::tracing::info;
use std::sync::Arc;
use world_chain_chainspec::WorldChainSpec;
use world_chain_cli::{
    Cli, WorldChainArgs, WorldChainNodeConfig, WorldChainRpcModuleValidator, WorldChainSpecParser,
};
use world_chain_evm::WorldChainEvmConfig;
use world_chain_kona::{InProcessEngineClient, KonaConfig, KonaServiceHandle};
use world_chain_node::{context::WorldChainDefaultContext, node::WorldChainNode};

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

    world_chain_node::init_version_metadata();

    let result = Cli::<WorldChainSpecParser, WorldChainArgs, WorldChainRpcModuleValidator>::parse()
        .run::<WorldChainNode<WorldChainDefaultContext>, _, _, _>(
            |mut builder, args| async move {
                info!(target: "reth::cli", "Launching node");

                let kona_args = args.kona.clone();
                let config: WorldChainNodeConfig = args.into_config(builder.config_mut())?;

                info!(target: "reth::cli", "Starting in Flashblocks mode");
                let node = WorldChainNode::<WorldChainDefaultContext>::new(config.clone());
                let NodeHandle {
                    node_exit_future,
                    node: full_node,
                } = builder.node(node).launch().await?;

                let kona_enabled = kona_args.as_ref().is_some_and(|k| k.enabled);
                if kona_enabled {
                    let kona_args = kona_args.expect("already checked");

                    let l1_rpc_url: url::Url = kona_args.l1_rpc_url.parse()?;
                    let l1_beacon_url: url::Url = kona_args.l1_beacon_url.parse()?;

                    let rollup_config = if let Some(path) = &kona_args.rollup_config_path {
                        let config_json = std::fs::read_to_string(path).map_err(|e| {
                            eyre::Report::msg(format!(
                                "failed to read rollup config from {}: {e}",
                                path.display()
                            ))
                        })?;
                        let config: kona_genesis::RollupConfig = serde_json::from_str(&config_json)
                            .map_err(|e| {
                                eyre::Report::msg(format!("failed to parse rollup config: {e}"))
                            })?;
                        std::sync::Arc::new(config)
                    } else {
                        return Err(eyre::Report::msg(
                            "--kona.rollup-config is required when --kona.enabled is set",
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
                    let payload_store = PayloadStore::new(full_node.payload_builder_handle.clone());
                    let l1_provider = alloy_provider::RootProvider::new_http(l1_rpc_url);

                    // L2 RPC provider for block reads / proofs. Points at reth's own HTTP endpoint
                    // so we get proper RPC type conversion for free.
                    // TODO(kona-integration): Extract actual HTTP port from reth's launched config.
                    let l2_rpc_url: url::Url = "http://127.0.0.1:8545".parse()?;
                    let l2_rpc = alloy_provider::RootProvider::<op_alloy_network::Optimism>::new_http(
                        l2_rpc_url,
                    );

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

                    let mut kona_handle = KonaServiceHandle::spawn(
                        kona_config,
                        engine_client,
                        l2_auth_rpc_url,
                        jwt_secret,
                    )
                    .await?;

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
            },
            |chain_spec: Arc<WorldChainSpec>| {
                (
                    WorldChainEvmConfig::optimism(chain_spec.clone()),
                    Arc::new(OpBeaconConsensus::new(chain_spec)),
                )
            },
        );

    if let Err(err) = result {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}
