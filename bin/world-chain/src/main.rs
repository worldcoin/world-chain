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
use world_chain_exex::{install_op_proposer_exex, ProposerCliArgs};
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

                // Snapshot the proposer args + datadir before `into_config`
                // moves `args`. When enabled, the ExEx is installed below and
                // runs alongside the node; when disabled the closure is never
                // invoked, courtesy of `install_exex_if`.
                let proposer_args: ProposerCliArgs = args.proposer.clone();
                let proposer_enabled = proposer_args.enabled;
                let proposer_datadir = builder
                    .config()
                    .datadir()
                    .data_dir()
                    .join("op-proposer");

                let config: WorldChainNodeConfig = args.into_config(builder.config_mut())?;

                info!(target: "reth::cli", "Starting in Flashblocks mode");
                let node = WorldChainNode::<WorldChainDefaultContext>::new(config.clone());

                if proposer_enabled {
                    info!(
                        target: "reth::cli",
                        datadir = %proposer_datadir.display(),
                        "Installing OP Proposer ExEx",
                    );
                }

                let NodeHandle {
                    node_exit_future,
                    node: _node,
                } = builder
                    .node(node)
                    .install_exex_if(proposer_enabled, "op-proposer", move |ctx| async move {
                        // `install_exex_if` expects `Result<Future<Result<()>>>`.
                        // The outer Ok wraps construction errors; the inner
                        // future is the long-running ExEx task. Inside we
                        // convert `OpProposerError` to `eyre::Report` so it
                        // chains correctly into the reth error type.
                        Ok(async move {
                            install_op_proposer_exex(ctx, proposer_args, proposer_datadir)
                                .await
                                .map_err(eyre::eyre::Report::from)
                        })
                    })
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
