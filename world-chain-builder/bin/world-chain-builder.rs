use clap::Parser;
use reth_optimism_cli::chainspec::OpChainSpecParser;
use reth_optimism_cli::Cli;
use world_chain_builder::exex::pbh_exex;
use world_chain_builder::node::args::ExtArgs;
use world_chain_builder::node::builder::WorldChainBuilder;
use world_chain_builder::pbh::db::load_world_chain_db;

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
        std::env::set_var("RUST_LOG", "trace,reth=debug");
    }

    if let Err(err) =
        Cli::<OpChainSpecParser, ExtArgs>::parse().run(|builder, ext_args| async move {
            let data_dir = builder.config().datadir();
            let db =
                load_world_chain_db(data_dir.data_dir(), ext_args.builder_args.clear_nullifiers)?;

            let handle: reth::builder::NodeHandle<reth::builder::NodeAdapter<reth::api::FullNodeTypesAdapter<reth::api::NodeTypesWithDBAdapter<WorldChainBuilder, std::sync::Arc<reth_db::DatabaseEnv>>, reth_provider::providers::BlockchainProvider<reth::api::NodeTypesWithDBAdapter<WorldChainBuilder, std::sync::Arc<reth_db::DatabaseEnv>>>>, reth::builder::components::Components<reth::api::FullNodeTypesAdapter<reth::api::NodeTypesWithDBAdapter<WorldChainBuilder, std::sync::Arc<reth_db::DatabaseEnv>>, reth_provider::providers::BlockchainProvider<reth::api::NodeTypesWithDBAdapter<WorldChainBuilder, std::sync::Arc<reth_db::DatabaseEnv>>>>, reth::transaction_pool::Pool<reth::transaction_pool::TransactionValidationTaskExecutor<world_chain_builder::pool::validator::WorldChainTransactionValidator<reth_provider::providers::BlockchainProvider<reth::api::NodeTypesWithDBAdapter<WorldChainBuilder, std::sync::Arc<reth_db::DatabaseEnv>>>, world_chain_builder::pool::tx::WorldChainPooledTransaction>>, world_chain_builder::pool::ordering::WorldChainOrdering<world_chain_builder::pool::tx::WorldChainPooledTransaction>, reth::transaction_pool::blobstore::DiskFileBlobStore>, reth_optimism_evm::OptimismEvmConfig, reth_optimism_evm::OpExecutorProvider, std::sync::Arc<dyn Consensus>, reth_optimism_node::engine::OptimismEngineValidator>>, world_chain_builder::node::builder::WorldChainAddOns> = builder
                .node(WorldChainBuilder::new(ext_args.clone(), db.clone())?)
                .install_exex("PBHExEx", move |ctx| pbh_exex(ctx, db))
                .launch()
                .await?;

            handle.node_exit_future.await
        })
    {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}
