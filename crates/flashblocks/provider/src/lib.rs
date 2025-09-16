#![cfg_attr(not(test), warn(unused_crate_dependencies))]

pub use reth_chain_state::CanonicalInMemoryState;
use reth_node_api::NodePrimitives;
use reth_provider::providers::{BlockchainProvider, ProviderNodeTypes};

pub trait InMemoryState {
    type Primitives: NodePrimitives;

    fn in_memory_state(&self) -> CanonicalInMemoryState<Self::Primitives>;
}

impl<N> InMemoryState for BlockchainProvider<N>
where
    N: ProviderNodeTypes,
{
    type Primitives = <N as reth_node_api::NodeTypes>::Primitives;

    fn in_memory_state(&self) -> CanonicalInMemoryState<Self::Primitives> {
        self.canonical_in_memory_state()
    }
}
