//! World Chain node-level wiring of all builder extensions.
//!
//! Public entry-point is [`install_worldchain_extensions`] — called from
//! `bin/world-chain::main` with `(builder, args)`, returns the builder with
//! every World-Chain-specific ExEx / extension installed. Currently:
//!
//! * **OP Proposer ExEx** — installed if `args.proposer.enabled`. See
//!   [`world_chain_exex`].
//!
//! Keeping the wiring here (instead of `bin/world-chain`) means the binary's
//! `main` is a single call and the provider trait bounds / `install_exex_if`
//! plumbing live alongside the rest of the node-level glue.
//!
//! Why these are builder extensions and not part of [`WorldChainAddOns`]:
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
use reth_node_core::node_config::NodeConfig;
use world_chain_chainspec::WorldChainSpec;
use world_chain_cli::WorldChainArgs;
use world_chain_exex::{ProposerCliArgs, ProviderBounds, install_op_proposer_exex};

/// Install every World-Chain-specific builder extension.
///
/// Reads what it needs from `args` + the builder's config (notably the
/// datadir for default proposer state), then chains the relevant
/// `install_*` calls. Returns the builder so the caller can chain
/// `.launch().await?` directly.
///
/// Today this only wires the OP Proposer ExEx; future World-Chain ExExes
/// (e.g. a state-bridge propagator) should be added here so `main` stays a
/// single call site.
pub fn install_worldchain_extensions<T, CB, AO>(
    builder: WithLaunchContext<NodeBuilderWithComponents<T, CB, AO>>,
    args: &WorldChainArgs,
) -> WithLaunchContext<NodeBuilderWithComponents<T, CB, AO>>
where
    T: FullNodeTypes<Types: NodeTypes<ChainSpec = WorldChainSpec>>,
    CB: NodeComponentsBuilder<T>,
    AO: RethRpcAddOns<NodeAdapter<T, CB::Components>>,
    NodeAdapter<T, CB::Components>: FullNodeComponents,
    <NodeAdapter<T, CB::Components> as FullNodeTypes>::Provider: ProviderBounds,
{
    let proposer_datadir = proposer_datadir(builder.config());
    builder.install_op_proposer(args.proposer.clone(), proposer_datadir)
}

/// Default proposer MDBX directory: `<reth-datadir>/op-proposer`.
fn proposer_datadir(cfg: &NodeConfig<WorldChainSpec>) -> PathBuf {
    cfg.datadir().data_dir().join("op-proposer")
}

/// Builder-extension trait: `.install_op_proposer(args, datadir)`.
///
/// No-op when `args.enabled` is false — the ExEx is not installed at all
/// (not even a draining stub), via [`install_exex_if`](
/// reth_node_builder::WithLaunchContext::install_exex_if).
pub trait OpProposerInstall: Sized {
    /// Install the OP Proposer ExEx if `args.enabled`, using `fallback_datadir`
    /// when the user hasn't passed `--proposer.datadir`.
    fn install_op_proposer(self, args: ProposerCliArgs, fallback_datadir: PathBuf) -> Self;
}

impl<T, CB, AO> OpProposerInstall for WithLaunchContext<NodeBuilderWithComponents<T, CB, AO>>
where
    T: FullNodeTypes,
    CB: NodeComponentsBuilder<T>,
    AO: RethRpcAddOns<NodeAdapter<T, CB::Components>>,
    NodeAdapter<T, CB::Components>: FullNodeComponents,
    <NodeAdapter<T, CB::Components> as FullNodeTypes>::Provider: ProviderBounds,
{
    fn install_op_proposer(self, args: ProposerCliArgs, fallback_datadir: PathBuf) -> Self {
        let enabled = args.enabled;
        self.install_exex_if(enabled, "op-proposer", move |ctx| async move {
            // `install_exex_if` wants `Result<Future<Result<()>>>`: the outer
            // Result reports setup failure (we have none here — config
            // translation happens inside the ExEx), and the inner future is
            // the long-running ExEx task. Convert `OpProposerError` to
            // `eyre::Report` so reth's error type accepts it.
            Ok(async move {
                install_op_proposer_exex(ctx, args, fallback_datadir)
                    .await
                    .map_err(eyre::eyre::Report::from)
            })
        })
    }
}
