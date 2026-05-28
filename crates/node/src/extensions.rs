//! World Chain node-level wiring of all builder extensions.

use std::path::PathBuf;

use reth_node_api::{FullNodeComponents, FullNodeTypes, NodeTypes};
use reth_node_builder::{
    NodeAdapter, NodeBuilderWithComponents, NodeComponentsBuilder, WithLaunchContext,
    rpc::RethRpcAddOns,
};
use world_chain_chainspec::WorldChainSpec;
use world_chain_cli::WorldChainArgs;
use world_chain_exex::{ProviderBounds, install_op_proposer_exex};

pub trait WorldChainNodeExtensions: Sized {
    /// Installer for World Chain Node Extensions.
    fn install_extensions(self, args: &WorldChainArgs) -> Self;
}

impl<T, CB, AO> WorldChainNodeExtensions for WithLaunchContext<NodeBuilderWithComponents<T, CB, AO>>
where
    T: FullNodeTypes<Types: NodeTypes<ChainSpec = WorldChainSpec>>,
    CB: NodeComponentsBuilder<T>,
    AO: RethRpcAddOns<NodeAdapter<T, CB::Components>>,
    NodeAdapter<T, CB::Components>: FullNodeComponents,
    <NodeAdapter<T, CB::Components> as FullNodeTypes>::Provider: ProviderBounds,
{
    fn install_extensions(self, args: &WorldChainArgs) -> Self {
        let proposer_datadir = self.config().datadir().data_dir().join("op-proposer");
        install_op_proposer(self, args.proposer.clone(), proposer_datadir)
    }
}

fn install_op_proposer<T, CB, AO>(
    builder: WithLaunchContext<NodeBuilderWithComponents<T, CB, AO>>,
    args: world_chain_exex::ProposerCliArgs,
    fallback_datadir: PathBuf,
) -> WithLaunchContext<NodeBuilderWithComponents<T, CB, AO>>
where
    T: FullNodeTypes,
    CB: NodeComponentsBuilder<T>,
    AO: RethRpcAddOns<NodeAdapter<T, CB::Components>>,
    NodeAdapter<T, CB::Components>: FullNodeComponents,
    <NodeAdapter<T, CB::Components> as FullNodeTypes>::Provider: ProviderBounds,
{
    let enabled = args.enabled;
    builder.install_exex_if(enabled, "op-proposer", move |ctx| async move {
        Ok(async move {
            install_op_proposer_exex(ctx, args, fallback_datadir)
                .await
                .map_err(eyre::eyre::Report::from)
        })
    })
}
