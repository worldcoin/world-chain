use clap::Parser;
use reth_node_builder::NodeHandle;
use reth_optimism_cli::{chainspec::OpChainSpecParser, Cli};
use reth_optimism_node::OpDAConfig;
use reth_tracing::tracing::info;
use world_chain_builder_node::args::NodeContextType;
use world_chain_builder_node::context::{BasicContext, FlashblocksContext};
use world_chain_builder_node::node::WorldChainNodeConfig;
use world_chain_builder_node::FlashblocksEthApiExtServer;
use world_chain_builder_node::{args::WorldChainArgs, node::WorldChainNode};
use world_chain_builder_rpc::EthApiExtServer;
use world_chain_builder_rpc::SequencerClient;
use world_chain_builder_rpc::WorldChainEthApiExt;

#[cfg(all(feature = "jemalloc", unix))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() {
    dotenvy::dotenv().ok();

    reth_cli_util::sigsegv_handler::install();
    eyre::install().unwrap();

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly provided.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "1");
    }

    // Set default log level
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "info,reth=info");
    }

    if let Err(err) =
        Cli::<OpChainSpecParser, WorldChainArgs>::parse().run(|builder, args| async move {
            info!(target: "reth::cli", "Launching node");
            let node_config = WorldChainNodeConfig {
                args: args.clone(),
                da_config: OpDAConfig::default(),
            };

            let node_context = node_config.clone().into();

            match node_context {
                NodeContextType::Basic => {
                    info!(target: "reth::cli", "Starting in Basic mode");
                    let node = WorldChainNode::<BasicContext>::new(node_config.clone());
                    let NodeHandle {
                        node_exit_future,
                        node: _node,
                    } = builder
                        .node(node)
                        .extend_rpc_modules(move |ctx| {
                            let provider = ctx.provider().clone();
                            let pool = ctx.pool().clone();
                            let sequencer_client = args.rollup.sequencer.map(SequencerClient::new);
                            let eth_api_ext =
                                WorldChainEthApiExt::new(pool, provider, sequencer_client);
                            ctx.modules.replace_configured(eth_api_ext.into_rpc())?;
                            Ok(())
                        })
                        .launch()
                        .await?;
                    node_exit_future.await?;
                }
                NodeContextType::Flashblocks => {
                    info!(target: "reth::cli", "Starting in Flashblocks mode");
                    let node = WorldChainNode::<FlashblocksContext>::new(node_config);
                    let NodeHandle {
                        node_exit_future,
                        node: _node,
                    } = builder
                        .node(node)
                        .extend_rpc_modules(move |ctx| {
                            let provider = ctx.provider().clone();
                            let pool = ctx.pool().clone();
                            let sequencer_client = args.rollup.sequencer.map(SequencerClient::new);
                            let eth_api_ext =
                                WorldChainEthApiExt::new(pool, provider, sequencer_client);
                            ctx.modules.replace_configured(eth_api_ext.into_rpc())?;

                            let eth_api = ctx.registry.eth_api().clone();
                            ctx.modules.replace_configured(eth_api.into_rpc())?;
                            Ok(())
                        })
                        .launch()
                        .await?;
                    node_exit_future.await?;
                }
            }

            Ok(())
        })
    {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}
