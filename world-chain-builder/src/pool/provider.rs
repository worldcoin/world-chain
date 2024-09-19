use reth_db::Database;
use reth_node_builder::NodeTypesWithDB;
use reth_provider::providers::{BlockchainProvider, ProviderNodeTypes};
use reth_provider::{DatabaseProviderFactory, DatabaseProviderRW, ProviderFactory, ProviderResult};

/// Database provider factory read write.
/// Not sure why this trait is missing from reth. Perhaps we can upstream.
pub trait DatabaseProviderFactoryRW<DB: Database, Spec>: DatabaseProviderFactory {
    /// Create new read-only database provider.
    fn database_provider_rw(&self) -> ProviderResult<DatabaseProviderRW<DB, Spec>>;
}

impl<N: NodeTypesWithDB + ProviderNodeTypes, Spec> DatabaseProviderFactoryRW<N::DB, Spec>
    for ProviderFactory<N>
{
    fn database_provider_rw(&self) -> ProviderResult<DatabaseProviderRW<N::DB, Spec>> {
        todo!()
        // self.provider_rw()
    }
}

impl<N: NodeTypesWithDB + ProviderNodeTypes, Spec> DatabaseProviderFactoryRW<N::DB, Spec>
    for BlockchainProvider<N>
{
    fn database_provider_rw(&self) -> ProviderResult<DatabaseProviderRW<N::DB, Spec>> {
        todo!()
    }
}
