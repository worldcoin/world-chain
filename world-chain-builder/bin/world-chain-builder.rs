use clap::Parser;
use reth_optimism_cli::Cli;
use world_chain_builder::args::ExtArgs;
use world_chain_builder::node::WorldChainBuilder;

#[cfg(all(feature = "jemalloc", unix))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() {
    reth_cli_util::sigsegv_handler::install();
    eyre::install().unwrap();

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly provided.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "1");
    }

    // Set default log level
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "trace,reth=debug");
    }

    if let Err(err) = Cli::<ExtArgs>::parse().run(|builder, builder_args| async move {
        let handle = builder
            .node(WorldChainBuilder::new(builder_args.clone()))
            .launch()
            .await?;

        handle.node_exit_future.await
    }) {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}
