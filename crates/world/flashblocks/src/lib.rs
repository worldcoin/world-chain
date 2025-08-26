#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::type_complexity)]

pub mod builder;
pub mod payload;
pub mod primitives;
pub mod rpc;

pub use builder::{
    traits::context::PayloadBuilderCtx, traits::context_builder::PayloadBuilderCtxBuilder,
    FlashblockBuilder, FlashblocksPayloadBuilder,
};
use reth_chain_state::CanonicalInMemoryState;
use reth_provider::providers::{BlockchainProvider, ProviderNodeTypes};

pub trait InMemoryState<N>
where
    N: ProviderNodeTypes,
{
    fn in_memory_state(
        &self,
    ) -> CanonicalInMemoryState<<N as reth_node_api::NodeTypes>::Primitives>;
}

impl<N> InMemoryState<N> for BlockchainProvider<N>
where
    N: ProviderNodeTypes,
{
    fn in_memory_state(
        &self,
    ) -> CanonicalInMemoryState<<N as reth_node_api::NodeTypes>::Primitives> {
        self.canonical_in_memory_state()
    }
}
