use alloy_primitives::{Bytes, U256};
use serde::{Deserialize, Serialize};
use world_chain_builder_node::test_utils::{PBHTransactionTestContext, DEV_CHAIN_ID};

#[derive(Deserialize, Serialize, Clone, Default)]
pub struct PBHFixture {
    pub fixture: Vec<Bytes>,
}

/// Generates test fixtures for PBH transactions
pub async fn generate_test_fixture() -> PBHFixture {
    let mut test_fixture = PBHFixture::default();
    for i in 1..=4 {
        for j in 0..=29 {
            test_fixture.fixture.push(
                PBHTransactionTestContext::raw_pbh_tx_bytes(
                    i,
                    j,
                    j.into(),
                    U256::from(j),
                    DEV_CHAIN_ID,
                )
                .await,
            );
        }
    }

    test_fixture
}
