use crate::{DEV_WORLD_ID, PBH_DEV_ENTRYPOINT, PBH_DEV_SIGNATURE_AGGREGATOR};
use reth_optimism_node::txpool::OpTransactionValidator;
use reth_primitives::EthPrimitives;
use reth_transaction_pool::blobstore::InMemoryBlobStore;
use reth_transaction_pool::validate::EthTransactionValidatorBuilder;
use revm_primitives::U256;
use world_chain_provider::{CanonicalInMemoryState, InMemoryState};

use crate::mock::{ExtendedAccount, MockEthProvider};
use world_chain_pool::root::WorldChainRootValidator;
use world_chain_pool::tx::WorldChainPooledTransaction;
use world_chain_pool::validator::{
    WorldChainTransactionValidator, MAX_U16, PBH_GAS_LIMIT_SLOT, PBH_NONCE_LIMIT_SLOT,
};

pub fn world_chain_validator(
) -> WorldChainTransactionValidator<MockEthProvider, WorldChainPooledTransaction> {
    let client = MockEthProvider::default();

    let validator = EthTransactionValidatorBuilder::new(client.clone())
        .no_shanghai()
        .no_cancun()
        .build(InMemoryBlobStore::default());
    let validator = OpTransactionValidator::new(validator).require_l1_data_gas_fee(false);
    let root_validator = WorldChainRootValidator::new(client, DEV_WORLD_ID).unwrap();
    validator.client().add_account(
        PBH_DEV_ENTRYPOINT,
        ExtendedAccount::new(0, alloy_primitives::U256::ZERO).extend_storage(vec![
            (PBH_GAS_LIMIT_SLOT.into(), U256::from(15000000)),
            (
                PBH_NONCE_LIMIT_SLOT.into(),
                ((MAX_U16 - U256::from(1)) << U256::from(160)),
            ),
        ]),
    );
    WorldChainTransactionValidator::new(
        validator,
        root_validator,
        PBH_DEV_ENTRYPOINT,
        PBH_DEV_SIGNATURE_AGGREGATOR,
    )
    .expect("failed to create world chain validator")
}

impl InMemoryState for MockEthProvider {
    type Primitives = EthPrimitives;

    fn in_memory_state(&self) -> CanonicalInMemoryState<Self::Primitives> {
        unimplemented!()
    }
}
