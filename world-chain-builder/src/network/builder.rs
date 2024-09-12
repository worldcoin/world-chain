use reth_chainspec::ChainSpec;
use reth_network::{NetworkHandle, NetworkManager};
use reth_node_builder::components::NetworkBuilder;
use reth_node_builder::{BuilderContext, FullNodeTypes, NodeTypes};
use reth_transaction_pool::TransactionPool;

#[derive(Debug, Default, Clone)]
pub struct WcNetworkBuilder {
    /// Disable transaction pool gossip
    pub disable_txpool_gossip: bool,
    /// Disable discovery v4
    pub disable_discovery_v4: bool,
}

impl<Node, Pool> NetworkBuilder<Node, Pool> for WcNetworkBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec = ChainSpec>>,
    Pool: TransactionPool + Unpin + 'static,
{
    async fn build_network(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<NetworkHandle> {
        let Self {
            disable_txpool_gossip,
            disable_discovery_v4,
        } = self;

        let args = &ctx.config().network;
        let network_builder = ctx
            .network_config_builder()?
            // apply discovery settings
            .apply(|mut builder| {
                let rlpx_socket = (args.addr, args.port).into();
                if disable_discovery_v4 || args.discovery.disable_discovery {
                    builder = builder.disable_discv4_discovery();
                }
                if !args.discovery.disable_discovery {
                    builder = builder.discovery_v5(
                        args.discovery.discovery_v5_builder(
                            rlpx_socket,
                            ctx.config()
                                .network
                                .resolved_bootnodes()
                                .or_else(|| ctx.chain_spec().bootnodes())
                                .unwrap_or_default(),
                        ),
                    );
                }

                builder
            });

        let mut network_config = ctx.build_network_config(network_builder);

        // When `sequencer_endpoint` is configured, the node will forward all transactions to a
        // Sequencer node for execution and inclusion on L1, and disable its own txpool
        // gossip to prevent other parties in the network from learning about them.
        network_config.tx_gossip_disabled = disable_txpool_gossip;

        let network = NetworkManager::builder(network_config).await?;

        let handle = ctx.start_network(network, pool);

        Ok(handle)
    }
}
