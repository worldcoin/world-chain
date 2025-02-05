use alloy_network::eip2718::Encodable2718;
use alloy_primitives::{Address, Bytes};
use alloy_rpc_types_eth::UserOperation;
use reth_e2e_test_utils::transaction::TransactionTestContext;
use serde::{Deserialize, Serialize};
use world_chain_builder_node::test_utils::{raw_pbh_multicall_bytes, tx, DEV_CHAIN_ID};
use world_chain_builder_pool::test_utils::signer;

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct TransactionFixtures {
    pub pbh: Vec<Bytes>,
    pub pbh_user_operations: Vec<UserOperation>,
    pub eip1559: Vec<Bytes>,
}

/// Generates test fixtures for PBH transactions
pub async fn generate_fixture(size: u32) -> TransactionFixtures {
    let mut test_fixture = TransactionFixtures::default();
    for i in 1..=5 {
        for j in 0..size {
            test_fixture
                .pbh
                .push(raw_pbh_multicall_bytes(i, j as u8, j as u64, DEV_CHAIN_ID).await);
        }
    }

    for j in size..=size + 2 {
        let tx = tx(DEV_CHAIN_ID, None, j as u64, Address::with_last_byte(0x01));
        let envelope = TransactionTestContext::sign_tx(signer(1), tx).await;
        let raw_tx = envelope.encoded_2718();
        test_fixture.eip1559.push(raw_tx.into());
    }

    test_fixture
}
