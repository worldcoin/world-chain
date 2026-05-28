//! World Chain node-level wiring of all builder extensions.
//!
//! Public surface is a single extension trait — [`WorldChainExtensions`] —
//! adding `.install_worldchain_extensions(args)` to the reth
//! [`WithLaunchContext`] node builder. `bin/world-chain::main` calls it once;
//! everything else (provider trait bounds, `install_exex_if` plumbing,
//! default datadir derivation) lives here.
//!
//! Currently wires:
//! * **OP Proposer ExEx** — installed if `args.proposer.enabled`. See
//!   [`world_chain_exex`].
//!
//! Why these are builder extensions and not part of `WorldChainAddOns`:
//! `NodeAddOns::launch_add_ons` runs *after* the node is launched, while
//! `install_exex_if` is a pre-launch step on `NodeBuilder`. ExExes can only
//! be installed before launch, so the natural home is a method on the
//! builder chain.

use std::path::PathBuf;

use reth_node_api::{FullNodeComponents, FullNodeTypes, NodeTypes};
use reth_node_builder::{
    NodeAdapter, NodeBuilderWithComponents, NodeComponentsBuilder, WithLaunchContext,
    rpc::RethRpcAddOns,
};
use world_chain_chainspec::WorldChainSpec;
use world_chain_cli::WorldChainArgs;
use world_chain_exex::{ProviderBounds, install_op_proposer_exex};

/// Builder-extension trait: `.install_worldchain_extensions(&args)`.
///
/// Bundles every World-Chain-specific ExEx / pre-launch wiring into one
/// call so `main` stays a single line. Returns the builder, so callers
/// chain `.launch().await?` directly.
///
/// Today this only installs the OP Proposer ExEx (gated on
/// `args.proposer.enabled`); future World-Chain ExExes — e.g. a state-bridge
/// propagator — should be added inside the impl below.
pub trait WorldChainExtensions: Sized {
    /// Install all World-Chain builder extensions.
    fn install_worldchain_extensions(self, args: &WorldChainArgs) -> Self;
}

impl<T, CB, AO> WorldChainExtensions for WithLaunchContext<NodeBuilderWithComponents<T, CB, AO>>
where
    T: FullNodeTypes<Types: NodeTypes<ChainSpec = WorldChainSpec>>,
    CB: NodeComponentsBuilder<T>,
    AO: RethRpcAddOns<NodeAdapter<T, CB::Components>>,
    NodeAdapter<T, CB::Components>: FullNodeComponents,
    <NodeAdapter<T, CB::Components> as FullNodeTypes>::Provider: ProviderBounds,
{
    fn install_worldchain_extensions(self, args: &WorldChainArgs) -> Self {
        let proposer_datadir = self.config().datadir().data_dir().join("op-proposer");
        install_op_proposer(self, args.proposer.clone(), proposer_datadir)
    }
}

/// Inner helper: install the OP Proposer ExEx if `args.enabled`.
///
/// Pulled out into a free function (instead of another trait method) so the
/// public surface stays a single trait. The provider trait bounds are the
/// same as on the trait `impl` above.
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
        // `install_exex_if` wants `Result<Future<Result<()>>>`: the outer
        // Result reports setup failure (we have none here — config
        // translation happens inside the ExEx), and the inner future is the
        // long-running ExEx task. Convert `OpProposerError` to
        // `eyre::Report` so reth's error type accepts it.
        Ok(async move {
            install_op_proposer_exex(ctx, args, fallback_datadir)
                .await
                .map_err(eyre::eyre::Report::from)
        })
    })
}
