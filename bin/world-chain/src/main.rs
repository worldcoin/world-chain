use clap::Parser;
use eyre::config::HookBuilder;
use reth_node_builder::NodeHandle;
use reth_optimism_consensus::OpBeaconConsensus;
use reth_rpc_builder::config::RethRpcServerConfig;
use reth_tracing::tracing::info;
use std::sync::Arc;
use world_chain_chainspec::WorldChainSpec;
use world_chain_cli::{
    Cli, WorldChainArgs, WorldChainNodeConfig, WorldChainRpcModuleValidator, WorldChainSpecParser,
};
use world_chain_evm::WorldChainEvmConfig;
use world_chain_kona::{KonaConfig, KonaServiceHandle};
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

                let kona_enabled = kona_args.as_ref().is_some_and(|k| k.enabled);

                // Capture reth's real auth-server JWT secret before launch, while `builder` is
                // still owned. This is the secret kona must present on the Engine API.
                let kona_jwt_secret = if kona_enabled {
                    let jwt_path = builder.config().datadir().jwt();
                    Some(builder.config().rpc.auth_jwt_secret(jwt_path)?)
                } else {
                    None
                };

                let NodeHandle {
                    node_exit_future,
                    node: full_node,
                } = builder.node(node).launch().await?;

                if let (Some(kona_args), Some(jwt_secret)) = (kona_args, kona_jwt_secret) {
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
                        rollup_config,
                        l1_rpc_url,
                        l1_beacon_url,
                        l1_trust_rpc: kona_args.l1_trust_rpc,
                        l2_trust_rpc: false,
                        sequencer_mode: kona_args.sequencer,
                        sequencer_stopped: kona_args.sequencer_stopped,
                        sequencer_recovery_mode: kona_args.sequencer_recovery_mode,
                        conductor_rpc_url: kona_args.conductor_rpc.clone(),
                        l1_confs: kona_args.l1_confs,
                        p2p: kona_args.p2p,
                        rpc_addr: kona_args.rpc_addr,
                        rpc_port: kona_args.rpc_port,
                        rpc_enable_admin: kona_args.rpc_enable_admin,
                        rpc_enabled: !kona_args.rpc_disabled,
                        l1_slot_duration_override: kona_args.l1_slot_duration_override,
                    };

                    // Canonical kona constructs its own engine client internally and drives reth's
                    // execution engine over the standard authenticated Engine API. Point it at
                    // reth's launched auth RPC endpoint, authenticated with reth's real JWT secret.
                    let auth_addr = full_node.auth_server_handle().local_addr();
                    let l2_auth_rpc_url: url::Url = format!("http://{auth_addr}").parse()?;

                    let mut kona_handle =
                        KonaServiceHandle::spawn(kona_config, l2_auth_rpc_url, jwt_secret).await?;

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

                    // Keep the launched reth node alive for the duration of the select above.
                    drop(full_node);
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
