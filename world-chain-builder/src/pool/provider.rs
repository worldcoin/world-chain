use reth_db::Database;
use reth_node_builder::NodeTypesWithDB;
use reth_provider::providers::{BlockchainProvider, ProviderNodeTypes};
use reth_provider::{DatabaseProviderFactory, DatabaseProviderRW, ProviderFactory, ProviderResult};

/// Database provider factory read write.
/// Not sure why this trait is missing from reth. Perhaps we can upstream.
pub trait DatabaseProviderFactoryRW<DB: Database>: DatabaseProviderFactory<DB> {
    /// Create new read-only database provider.
    fn database_provider_rw(&self) -> ProviderResult<DatabaseProviderRW<DB>>;
}

impl<N: NodeTypesWithDB + ProviderNodeTypes> DatabaseProviderFactoryRW<N::DB>
    for ProviderFactory<N>
{
    fn database_provider_rw(&self) -> ProviderResult<DatabaseProviderRW<N::DB>> {
        self.provider_rw()
    }
}

impl<N: NodeTypesWithDB + ProviderNodeTypes> DatabaseProviderFactoryRW<N::DB>
    for BlockchainProvider<N>
{
    fn database_provider_rw(&self) -> ProviderResult<DatabaseProviderRW<N::DB>> {
        todo!()
    }
}
