use std::fs::File;

use alloy_primitives::{Bytes, U256};
use eyre::eyre::Result;
use serde::{Deserialize, Serialize};
use world_chain_builder_node::tests::e2e::PBHTransactionTestContext;

pub const FIXTURES_DIR: &str = "../../../../devnet/fixtures/fixture.json";

#[derive(Deserialize, Serialize, Clone, Default)]
pub struct PBHFixture {
    pub fixture: Vec<Bytes>,
}

/// Generates test fixtures for PBH transactions
pub async fn generate_test_fixture() -> Result<()> {
    let mut test_fixture = PBHFixture::default();
    for i in 0..=5 {
        for j in 0..=29 {
            test_fixture.fixture.push(
                PBHTransactionTestContext::raw_pbh_tx_bytes(i, j, j.into(), U256::from(j)).await,
            );
        }
    }

    serde_json::to_writer(File::create(FIXTURES_DIR)?, &test_fixture)?;
    Ok(())
}
