use clap::Parser;
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use reth_optimism_cli::Cli;
use reth_tracing::tracing::info;
use tokio::sync::broadcast;
use world_chain_builder_chainspec::spec::WorldChainChainSpecParser;
use world_chain_builder_node::flashblocks::WorldChainFlashblocksNode;
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
        Cli::<WorldChainChainSpecParser, WorldChainArgs>::parse().run(|builder, args| async move {
            info!(target: "reth::cli", "Launching node");

            if let Some(flashblocks_args) = args.flashblocks_args.clone() {
                let authorizer_vk = flashblocks_args
                    .flashblocks_authorizor_vk
                    .unwrap_or(flashblocks_args.flashblocks_builder_sk.verifying_key());

                let (flashblocks_tx, _) = broadcast::channel(100);

                let flashblocks_handle = FlashblocksHandle::new(
                    authorizer_vk,
                    flashblocks_args.flashblocks_builder_sk.clone(),
                    flashblocks_tx.clone(),
                );

                let node = WorldChainFlashblocksNode::new(args.clone(), flashblocks_handle);
                let handle = builder
                    .node(node)
                    .extend_rpc_modules(move |ctx| {
                        let provider = ctx.provider().clone();
                        let pool = ctx.pool().clone();
                        let sequencer_client = args.rollup_args.sequencer.map(SequencerClient::new);
                        let eth_api_ext =
                            WorldChainEthApiExt::new(pool, provider, sequencer_client);
                        ctx.modules.replace_configured(eth_api_ext.into_rpc())?;
                        Ok(())
                    })
                    .launch()
                    .await?;

                handle.node_exit_future.await
            } else {
                let node = WorldChainNode::new(args.clone());
                let handle = builder
                    .node(node)
                    .extend_rpc_modules(move |ctx| {
                        let provider = ctx.provider().clone();
                        let pool = ctx.pool().clone();
                        let sequencer_client = args.rollup_args.sequencer.map(SequencerClient::new);
                        let eth_api_ext =
                            WorldChainEthApiExt::new(pool, provider, sequencer_client);
                        ctx.modules.replace_configured(eth_api_ext.into_rpc())?;
                        Ok(())
                    })
                    .launch()
                    .await?;

                handle.node_exit_future.await
            }
        })
    {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}
