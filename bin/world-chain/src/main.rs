use clap::Parser;
use eyre::config::HookBuilder;
use reth_node_builder::NodeHandle;
use reth_optimism_consensus::OpBeaconConsensus;
use reth_tracing::tracing::info;
use std::sync::Arc;
use world_chain_chainspec::WorldChainSpec;
use world_chain_cli::{
    Cli, WorldChainArgs, WorldChainNodeConfig, WorldChainRpcModuleValidator, WorldChainSpecParser,
};
use world_chain_evm::WorldChainEvmConfig;
use world_chain_node::{
    WorldChainExtensions, context::WorldChainDefaultContext, node::WorldChainNode,
};

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
                let config: WorldChainNodeConfig = args.clone().into_config(builder.config_mut())?;

                info!(target: "reth::cli", "Starting in Flashblocks mode");
                let node = WorldChainNode::<WorldChainDefaultContext>::new(config.clone());

                let NodeHandle { node_exit_future, node: _node } = builder
                    .node(node)
                    .install_worldchain_extensions(&args)
                    .launch()
                    .await?;
                
                node_exit_future.await?;

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
