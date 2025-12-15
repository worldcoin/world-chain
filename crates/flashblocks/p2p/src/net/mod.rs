use crate::{
    monitor::PeerMonitor,
    protocol::handler::{FlashblocksHandle, FlashblocksP2PNetworkHandle, FlashblocksP2PProtocol},
};
use reth::chainspec::Hardforks;
use reth_eth_wire::NetPrimitivesFor;
use reth_ethereum::network::api::FullNetwork;
use reth_network::{NetworkProtocols, protocol::IntoRlpxSubProtocol};
use reth_node_api::{PrimitivesTy, TxTy};
use reth_node_builder::{
    BuilderContext,
    components::NetworkBuilder,
    node::{FullNodeTypes, NodeTypes},
};
use reth_transaction_pool::{PoolTransaction, TransactionPool};

#[derive(Debug)]
pub struct FlashblocksNetworkBuilder<T> {
    inner: T,
    flashblocks_p2p_handle: Option<FlashblocksHandle>,
}

impl<T> FlashblocksNetworkBuilder<T> {
    pub fn new(inner: T, flashblocks_p2p_handle: Option<FlashblocksHandle>) -> Self {
        Self {
            inner,
            flashblocks_p2p_handle,
        }
    }

    pub fn disabled(inner: T) -> Self {
        Self {
            inner,
            flashblocks_p2p_handle: None,
        }
    }
}

impl<T, Network, Node, Pool> NetworkBuilder<Node, Pool> for FlashblocksNetworkBuilder<T>
where
    T: NetworkBuilder<Node, Pool, Network = Network>,
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec: Hardforks>>,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TxTy<Node::Types>>>
        + Unpin
        + 'static,
    Network: FlashblocksP2PNetworkHandle
        + NetworkProtocols
        + FullNetwork<Primitives: NetPrimitivesFor<PrimitivesTy<Node::Types>>>,
{
    type Network = T::Network;

    async fn build_network(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<Self::Network> {
        let handle = self.inner.build_network(ctx, pool).await?;
        if let Some(flashblocks_handle) = self.flashblocks_p2p_handle {
            let flashblocks_rlpx = FlashblocksP2PProtocol {
                network: handle.clone(),
                handle: flashblocks_handle,
            };
            handle.add_rlpx_sub_protocol(flashblocks_rlpx.into_rlpx_sub_protocol());

            // Merge trusted peers from both CLI args and reth.toml config file
            let cli_peers = ctx.config().network.trusted_peers.iter();
            let toml_peers = ctx.reth_config().peers.trusted_nodes.iter();
            let all_trusted_peers = cli_peers.chain(toml_peers).map(|peer| peer.id);

            PeerMonitor::new(handle.clone())
                .with_initial_peers(all_trusted_peers)
                .run_on_task_executor(ctx.task_executor());
        }

        Ok(handle)
    }
}
