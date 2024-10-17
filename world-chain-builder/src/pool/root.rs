use std::{collections::BTreeMap, sync::Arc};

use parking_lot::RwLock;
use reth_primitives::SealedBlock;
use reth_provider::{BlockReaderIdExt, StateProviderFactory};
use revm_primitives::{address, Address, U256};
use semaphore::Field;

use super::error::WorldChainTransactionPoolError;

/// The WorldID contract address.
pub const OP_WORLD_ID: Address = address!("42ff98c4e85212a5d31358acbfe76a621b50fc02");
/// The slot of the `_latestRoot` in the WorldID contract.
pub const LATEST_ROOT_SLOT: U256 = U256::from_limbs([1, 0, 0, 0]);
/// Root Expiration Period
pub const ROOT_EXPIRATION_WINDOW: u64 = 60 * 60; // 1 hour

/// A provider for managing and validating World Chain roots.
#[derive(Debug, Clone)]
pub struct RootProvider<Client>
where
    Client: StateProviderFactory + BlockReaderIdExt,
{
    /// The client used to aquire account state from the database.
    client: Client,
    /// A map of valid roots indexed by block timestamp.
    valid_roots: BTreeMap<u64, Field>,
    /// The timestamp of the latest valid root.
    latest_valid_timestamp: u64,
    /// The latest root
    latest_root: Field,
}

/// TODO: Think through reorg scenarios
impl<Client> RootProvider<Client>
where
    Client: StateProviderFactory + BlockReaderIdExt,
{
    /// Creates a new [`RootProvider`] instance.
    ///
    /// # Arguments
    ///
    /// * `client` - The client used to aquire account state from the database.
    pub fn new(client: Client) -> Result<Self, WorldChainTransactionPoolError> {
        let mut this = Self {
            client,
            valid_roots: BTreeMap::new(),
            latest_valid_timestamp: 0,
            latest_root: Field::ZERO,
        };

        // If we have a state provider, we can try to load the latest root from the state.
        if let Ok(latest) = this.client.last_block_number() {
            let block = this.client.block(latest.into())?;
            if let Some(block) = block {
                let state = this.client.state_by_block_hash(block.hash_slow())?;
                let latest_root = state.storage(OP_WORLD_ID, LATEST_ROOT_SLOT.into())?;
                if let Some(latest) = latest_root {
                    this.latest_root = latest;
                    this.valid_roots.insert(block.timestamp, latest);
                };
            }
        }
        Ok(this)
    }

    /// Commits any changes to the state.
    ///
    /// # Arguments
    ///
    /// * `block` - The new block to be committed.
    fn on_new_block(&mut self, block: &SealedBlock) -> Result<(), WorldChainTransactionPoolError> {
        let state = self
            .client
            .state_by_block_hash(block.hash())
            .map_err(WorldChainTransactionPoolError::RootProvider)?;
        let root = state
            .storage(OP_WORLD_ID, LATEST_ROOT_SLOT.into())
            .map_err(WorldChainTransactionPoolError::RootProvider)?;
        self.latest_valid_timestamp = block.header.timestamp;
        if let Some(root) = root {
            self.valid_roots.insert(block.header.timestamp, root);
        }

        self.prune_invalid();

        Ok(())
    }

    /// Prunes all roots from the cache that are not within the expiration window.
    fn prune_invalid(&mut self) {
        if self.latest_valid_timestamp > ROOT_EXPIRATION_WINDOW {
            self.valid_roots.retain(|timestamp, root| {
                *timestamp >= self.latest_valid_timestamp - ROOT_EXPIRATION_WINDOW
                    || *root == self.latest_root // Always keep the latest root
            });
        };
    }

    /// Returns a vector of all valid roots.
    ///
    /// # Returns
    ///
    /// A `Vec<Field>` containing all valid roots.
    fn roots(&self) -> Vec<Field> {
        self.valid_roots.values().cloned().collect()
    }
}

/// A validator for World Chain roots.
#[derive(Debug, Clone)]
pub struct WorldChainRootValidator<Client>
where
    Client: StateProviderFactory + BlockReaderIdExt,
{
    /// The [`RootProvider`] used for caching and managing roots.
    cache: Arc<RwLock<RootProvider<Client>>>,
}

impl<Client> WorldChainRootValidator<Client>
where
    Client: StateProviderFactory + BlockReaderIdExt,
{
    /// Creates a new [`WorldChainRootValidator`] instance.
    ///
    /// # Arguments
    ///
    /// * `client` - The client used for state and block operations.
    pub fn new(client: Client) -> Result<Self, WorldChainTransactionPoolError> {
        let cache = RootProvider::new(client)?;

        Ok(Self {
            cache: Arc::new(RwLock::new(cache)),
        })
    }

    /// Validates a given root.
    ///
    /// # Arguments
    ///
    /// * `root` - The root to be validated.
    ///
    /// # Returns
    ///
    /// A boolean indicating whether the root is valid.
    pub fn validate_root(&self, root: Field) -> bool {
        self.cache.read().roots().contains(&root)
    }

    /// Commits a new block to the validator.
    ///
    /// # Arguments
    ///
    /// * `block` - The new block to be committed.
    pub fn on_new_block(&self, block: &SealedBlock) {
        if let Err(e) = self.cache.write().on_new_block(block) {
            tracing::error!("Failed to commit new block: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use reth_primitives::Header;
    use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};

    use super::*;
    use reth_primitives::Block;
    pub fn world_chain_root_validator() -> eyre::Result<WorldChainRootValidator<MockEthProvider>> {
        let client = MockEthProvider::default();
        let root_validator = WorldChainRootValidator::new(client)?;
        Ok(root_validator)
    }

    fn add_block_with_root_with_timestamp(
        validator: &WorldChainRootValidator<MockEthProvider>,
        timestamp: u64,
        root: Field,
    ) {
        let header = Header {
            timestamp,
            ..Default::default()
        };
        let block = Block {
            header,
            ..Default::default()
        };
        validator.cache.read().client().add_account(
            OP_WORLD_ID,
            ExtendedAccount::new(0, U256::ZERO)
                .extend_storage(vec![(LATEST_ROOT_SLOT.into(), root)]),
        );
        validator
            .cache
            .read()
            .client()
            .add_block(block.hash_slow(), block.clone());
        let hash = block.hash_slow();
        let block = block.seal(hash);
        validator.on_new_block(&block);
    }

    #[test]
    fn test_validate_root() -> eyre::Result<()> {
        let validator = world_chain_root_validator()?;
        let root_1 = Field::from(1u64);
        let timestamp = 1000000000;
        add_block_with_root_with_timestamp(&validator, timestamp, root_1);
        assert!(validator.validate_root(root_1));
        let root_2 = Field::from(2u64);
        add_block_with_root_with_timestamp(&validator, timestamp + 3601, root_2);
        assert!(validator.validate_root(root_2));
        assert!(!validator.validate_root(root_1));
        let root_3 = Field::from(3u64);
        add_block_with_root_with_timestamp(&validator, timestamp + 3600 + 3600, root_3);
        assert!(validator.validate_root(root_3));
        assert!(validator.validate_root(root_2));
        assert!(!validator.validate_root(root_1));
        Ok(())
    }

    impl<Client> WorldChainRootValidator<Client>
    where
        Client: StateProviderFactory + BlockReaderIdExt,
    {
        pub fn set_client(&mut self, client: Client) {
            self.cache.write().set_client(client);
        }
    }

    impl<Client> RootProvider<Client>
    where
        Client: StateProviderFactory + BlockReaderIdExt,
    {
        pub fn set_client(&mut self, client: Client) {
            self.client = client;
        }

        pub fn client(&self) -> &Client {
            &self.client
        }
    }
}
