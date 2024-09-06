use clap::Parser;
use reth_node_optimism::{args::RollupArgs, OptimismNode};
use reth_optimism_cli::Cli;
use world_chain_builder::args::ExtArgs;
use world_chain_builder::node::WorldChainBuilder;

#[cfg(all(feature = "jemalloc", unix))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() {
    reth_cli_util::sigsegv_handler::install();

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly provided.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "1");
    }

    if let Err(err) = Cli::<ExtArgs>::parse().run(|builder, builder_args| async move {
        let handle = builder
            .node(WorldChainBuilder::new(builder_args.rollup_args.clone()))
            .launch()
            .await?;

        handle.node_exit_future.await
    }) {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}
