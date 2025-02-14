use reth::transaction_pool::blobstore::InMemoryBlobStore;
use reth::transaction_pool::validate::EthTransactionValidatorBuilder;
use reth_optimism_node::txpool::OpTransactionValidator;
use world_chain_builder_test_utils::{
    DEV_WORLD_ID, PBH_DEV_ENTRYPOINT, PBH_DEV_SIGNATURE_AGGREGATOR,
};

use crate::mock::MockEthProvider;
use crate::root::WorldChainRootValidator;
use crate::tx::WorldChainPooledTransaction;
use crate::validator::WorldChainTransactionValidator;

pub fn world_chain_validator(
) -> WorldChainTransactionValidator<MockEthProvider, WorldChainPooledTransaction> {
    let client = MockEthProvider::default();

    let validator = EthTransactionValidatorBuilder::new(client.clone())
        .no_shanghai()
        .no_cancun()
        .build(InMemoryBlobStore::default());
    let validator = OpTransactionValidator::new(validator).require_l1_data_gas_fee(false);
    let root_validator = WorldChainRootValidator::new(client, DEV_WORLD_ID).unwrap();
    WorldChainTransactionValidator::new(
        validator,
        root_validator,
        30,
        PBH_DEV_ENTRYPOINT,
        PBH_DEV_SIGNATURE_AGGREGATOR,
    )
}
