use clap::Parser;
use reth_node_builder::{engine_tree_config::TreeConfig, EngineNodeLauncher, Node};
use reth_optimism_cli::chainspec::OpChainSpecParser;
use reth_optimism_cli::Cli;
use reth_provider::providers::BlockchainProvider2;
use world_chain_builder_node::args::ExtArgs;
use world_chain_builder_node::node::WorldChainBuilder;
use world_chain_builder_rpc::{sequencer::SequencerClient, EthApiExtServer, WorldChainEthApiExt};

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
        Cli::<OpChainSpecParser, ExtArgs>::parse().run(|builder, builder_args| async move {
            let engine_tree_config = TreeConfig::default()
                .with_persistence_threshold(builder_args.rollup_args.persistence_threshold)
                .with_memory_block_buffer_target(
                    builder_args.rollup_args.memory_block_buffer_target,
                );
            let world_chain_node = WorldChainBuilder::new(builder_args.clone())?;
            let handle = builder
                .with_types_and_provider::<WorldChainBuilder, BlockchainProvider2<_>>()
                .with_components(world_chain_node.components_builder())
                .with_add_ons(world_chain_node.add_ons())
                .extend_rpc_modules(move |ctx| {
                    let provider = ctx.provider().clone();
                    let pool = ctx.pool().clone();
                    let sequencer_client = builder_args
                        .rollup_args
                        .sequencer_http
                        .map(SequencerClient::new);
                    let eth_api_ext = WorldChainEthApiExt::new(pool, provider, sequencer_client);
                    ctx.modules.replace_configured(eth_api_ext.into_rpc())?;
                    Ok(())
                })
                .launch_with_fn(|builder| {
                    let launcher = EngineNodeLauncher::new(
                        builder.task_executor().clone(),
                        builder.config().datadir(),
                        engine_tree_config,
                    );
                    builder.launch_with(launcher)
                })
                .await?;

            handle.node_exit_future.await
        })
    {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}
