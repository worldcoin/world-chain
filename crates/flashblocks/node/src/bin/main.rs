#![allow(missing_docs, rustdoc::missing_crate_level_docs)]
use clap::Parser;
use flashblocks_cli::FlashblocksArgs;
use flashblocks_node::FlashblocksNodeBuilder;
use flashblocks_p2p::protocol::handler::FlashblocksP2PProtocol;
use flashblocks_rpc::op::{FlashblocksOpApi, OpApiExtServer};
use reth_ethereum::network::{protocol::IntoRlpxSubProtocol, NetworkProtocols};
use reth_optimism_cli::{chainspec::OpChainSpecParser, Cli};
use reth_optimism_node::args::RollupArgs;
use tracing::info;

#[derive(Debug, Clone, clap::Args)]
struct FlashblocksNodeArgs {
    #[command(flatten)]
    pub rollup_args: RollupArgs,

    #[command(flatten)]
    pub flashblock_args: FlashblocksArgs,
}

pub fn main() {
    if let Err(err) =
        Cli::<OpChainSpecParser, FlashblocksNodeArgs>::parse().run(async move |builder, args| {
            let node = FlashblocksNodeBuilder {
                rollup: args.rollup_args.clone(),
                flashblocks: args.flashblock_args,
                da_config: Default::default(),
            }
            .build();

            let p2p_handle = node.flashblocks_handle.clone();

            info!(target: "reth::cli", "Launching Flashblocks RPC overlay node");
            let handle = builder
                .node(node)
                .extend_rpc_modules(move |ctx| {
                    ctx.modules
                        .replace_configured(FlashblocksOpApi.into_rpc())?;
                    Ok(())
                })
                .launch()
                .await?;

            let flashblocks_p2p_protocol =
                FlashblocksP2PProtocol::new(handle.node.network.clone(), p2p_handle);

            handle
                .node
                .network
                .add_rlpx_sub_protocol(flashblocks_p2p_protocol.into_rlpx_sub_protocol());

            handle.node_exit_future.await
        })
    {
        tracing::error!("Error: {err:?}");
        std::process::exit(1);
    }
}
