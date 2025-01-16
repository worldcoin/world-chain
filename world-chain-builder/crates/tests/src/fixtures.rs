use alloy_network::eip2718::Encodable2718;
use alloy_primitives::{Address, Bytes, U256};
use reth_e2e_test_utils::transaction::TransactionTestContext;
use serde::{Deserialize, Serialize};
use world_chain_builder_node::test_utils::{tx, PBHTransactionTestContext, DEV_CHAIN_ID};
use world_chain_builder_pool::test_utils::signer;

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct TransactionFixtures {
    pub pbh: Vec<Bytes>,
    pub eip1559: Vec<Bytes>,
}

/// Generates test fixtures for PBH transactions
pub async fn generate_fixture(size: u32) -> Vec<Bytes> {
    let mut test_fixture = TransactionFixtures::default();
    for i in 0..=5 {
        for j in 0..=size {
            test_fixture.pbh.push(
                PBHTransactionTestContext::raw_pbh_tx_bytes(
                    i,
                    j as u8,
                    j as u64,
                    U256::from(j),
                    DEV_CHAIN_ID,
                )
                .await,
            );
        }
    }

    test_fixture
}

/// Generates test fixture for standerd EIP-1559 transactions
pub async fn generate_eip1559_fixture(size: u32) -> Vec<Bytes> {
    let mut test_fixture = vec![];
    for i in 0..=5 {
        for j in 0..=size {
            let tx = tx(DEV_CHAIN_ID, None, j as u64, Address::with_last_byte(0x01));
            let envelope = TransactionTestContext::sign_tx(signer(i), tx).await;
            let raw_tx = envelope.encoded_2718();
            test_fixture.push(raw_tx.into());
        }
    }

    test_fixture
}
