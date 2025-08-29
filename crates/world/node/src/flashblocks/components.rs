use flashblocks_p2p::net::FlashblocksNetworkBuilder;
use reth_node_builder::components::ComponentsBuilder;
use rollup_boost::FlashblocksArgs;

use crate::flashblocks::service_builder::FlashblocksPayloadServiceBuilder;

pub struct FlashblocksComponentsBuilder<Node, PoolB, PayloadB, NetworkB, ExecB, ConsB> {
    pub args: Option<FlashblocksArgs>,
    pub components: ComponentsBuilder<Node, PoolB, PayloadB, NetworkB, ExecB, ConsB>,
}

impl<Node, PoolB, PayloadB, NetworkB, ExecB, ConsB>
    FlashblocksComponentsBuilder<Node, PoolB, PayloadB, NetworkB, ExecB, ConsB>
{
    pub fn new(
        args: Option<FlashblocksArgs>,
        components: ComponentsBuilder<Node, PoolB, PayloadB, NetworkB, ExecB, ConsB>,
    ) -> Self {
        FlashblocksComponentsBuilder { args, components }
    }

    pub fn build(
        self,
    ) -> ComponentsBuilder<
        Node,
        PoolB,
        FlashblocksPayloadServiceBuilder<PayloadB>,
        FlashblocksNetworkBuilder<NetworkB>,
        ExecB,
        ConsB,
    > {
        todo!()
    }
}
