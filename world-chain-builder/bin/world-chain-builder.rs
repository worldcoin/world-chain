use clap::Parser;
use reth_optimism_cli::chainspec::OpChainSpecParser;
use reth_optimism_cli::Cli;
use world_chain_builder::node::args::ExtArgs;
use world_chain_builder::node::builder::WorldChainBuilder;
use world_chain_builder::rpc::bundle::EthTransactionsExtServer;
use world_chain_builder::rpc::bundle::WorldChainEthApiExt;

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
            let data_dir = builder.config().datadir();
            // let handle = builder
            //     .node(WorldChainBuilder::new(
            //         builder_args.clone(),
            //         data_dir.data_dir(),
            //     )?)
            //     .extend_rpc_modules(move |ctx| {
            //         let provider = ctx.provider().clone();
            //         let pool = ctx.pool().clone();
            //         let eth_api_ext = WorldChainEthApiExt::new(pool, provider);
            //         ctx.modules.merge_configured(eth_api_ext.into_rpc())?;
            //         Ok(())
            //     })
            //     .launch()
            //     .await?;

            let engine_tree_config = TreeConfig::default()
                .with_persistence_threshold(rollup_args.persistence_threshold)
                .with_memory_block_buffer_target(rollup_args.memory_block_buffer_target);

            let handle = builder
                .with_types_and_provider::<OpNode, BlockchainProvider2<_>>()
                .with_components(OpNode::components(rollup_args.clone()))
                .with_add_ons(OpNode::new(rollup_args).add_ons())
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
