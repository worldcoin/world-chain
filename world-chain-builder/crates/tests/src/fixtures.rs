use alloy_network::eip2718::Encodable2718;
use alloy_primitives::{Address, Bytes};
use chrono::Datelike;
use reth_e2e_test_utils::transaction::TransactionTestContext;
use serde::{Deserialize, Serialize};
use world_chain_builder_node::test_utils::{raw_pbh_multicall_bytes, tx, DEV_CHAIN_ID};
use world_chain_builder_pbh::external_nullifier::ExternalNullifier;
use world_chain_builder_test_utils::{
    bindings::IEntryPoint::PackedUserOperation,
    utils::{signer, user_op},
};

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct TransactionFixtures {
    pub pbh_txs: Vec<Bytes>,
    pub pbh_user_operations: Vec<PackedUserOperation>,
    pub eip1559: Vec<Bytes>,
}

impl TransactionFixtures {
    pub async fn new() -> Self {
        let mut pbh_txs = Vec::new();
        for i in 0..255 {
            pbh_txs.push(raw_pbh_multicall_bytes(1, i as u8, i as u64, DEV_CHAIN_ID).await);
        }

        let mut pbh_user_operations = Vec::new();
        for i in 0..255 {
            let dt = chrono::Utc::now();
            let dt = dt.naive_local();
            let month = dt.month() as u8;
            let year = dt.year() as u16;

            let ext_nullifier = ExternalNullifier::v1(month, year, i);
            pbh_user_operations.push(user_op().acc(2).external_nullifier(ext_nullifier).call().0);
        }

        let mut eip1559 = Vec::new();
        for i in 0..2 {
            let tx = tx(DEV_CHAIN_ID, None, i as u64, Address::with_last_byte(0x01));
            let envelope = TransactionTestContext::sign_tx(signer(3), tx).await;
            let raw_tx = envelope.encoded_2718();
            eip1559.push(raw_tx.into());
        }

        Self {
            pbh_txs,
            pbh_user_operations,
            eip1559,
        }
    }
}
