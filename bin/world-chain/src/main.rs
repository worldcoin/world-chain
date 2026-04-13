use clap::Parser;
use eyre::config::HookBuilder;
use reth_node_builder::NodeHandle;
use reth_optimism_cli::{Cli, chainspec::OpChainSpecParser};
use reth_tracing::tracing::info;
use world_chain_cli::{WorldChainArgs, WorldChainNodeConfig};
use world_chain_node::{
    FlashblocksOpApi, OpApiExtServer, context::WorldChainDefaultContext, node::WorldChainNode,
};
use reth_optimism_evm::{OpEvmConfig, OpRethReceiptBuilder};
use reth_provider::ChainSpecProvider;
use world_chain_rpc::{
    EthApiExtServer, SequencerClient, WorldChainEthApiExt, WorldChainSimulate,
    WorldChainSimulateApiServer,
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

    if let Err(err) =
        Cli::<OpChainSpecParser, WorldChainArgs>::parse().run(|mut builder, args| async move {
            info!(target: "reth::cli", "Launching node");
            let config: WorldChainNodeConfig = args.into_config(builder.config_mut())?;

            info!(target: "reth::cli", "Starting in Flashblocks mode");
            let node = WorldChainNode::<WorldChainDefaultContext>::new(config.clone());
            let NodeHandle {
                node_exit_future,
                node: _node,
            } = builder
                .node(node)
                .extend_rpc_modules(move |ctx| {
                    let provider = ctx.provider().clone();
                    let pool = ctx.pool().clone();
                    let sequencer_client = config.args.rollup.sequencer.map(SequencerClient::new);
                    let eth_api_ext = WorldChainEthApiExt::new(pool, provider.clone(), sequencer_client);
                    ctx.modules.replace_configured(eth_api_ext.into_rpc())?;
                    ctx.modules
                        .replace_configured(FlashblocksOpApi.into_rpc())?;

                    // Register worldchain_simulateUserOp endpoint
                    let chain_spec = ctx.provider().chain_spec();
                    let evm_config = OpEvmConfig::new(chain_spec, OpRethReceiptBuilder::default());
                    let simulate_api = WorldChainSimulate::new(provider, evm_config);
                    ctx.modules.merge_configured(simulate_api.into_rpc())?;

                    Ok(())
                })
                .launch()
                .await?;
            node_exit_future.await?;

            Ok(())
        })
    {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}
