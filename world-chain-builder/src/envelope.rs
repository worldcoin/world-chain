use op_alloy_consensus::OpTxEnvelope;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum WorldChainTxEnvelope {
    Unverified(OpTxEnvelope),
    Verified(VerifiedTx),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedTx {
    pub tx: OpTxEnvelope,
    pub proof: Vec<u8>,
}

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use alloy_primitives::{hex, Address, Bytes, TxKind, B256, U256};
//     use op_alloy_consensus::TxDeposit;
//
//     #[test]
//     fn test_encode_decode_deposit() {
//         let tx = TxDeposit {
//             source_hash: B256::left_padding_from(&[0xde, 0xad]),
//             from: Address::left_padding_from(&[0xbe, 0xef]),
//             mint: Some(1),
//             gas_limit: 2,
//             to: TxKind::Call(Address::left_padding_from(&[3])),
//             value: U256::from(4_u64),
//             input: Bytes::from(vec![5]),
//             is_system_transaction: false,
//         };
//         let tx_envelope = OpTxEnvelope::Deposit(tx);
//         let encoded = tx_envelope.encoded_2718();
//         let decoded = OpTxEnvelope::decode_2718(&mut encoded.as_ref()).unwrap();
//         assert_eq!(encoded.len(), tx_envelope.encode_2718_len());
//         assert_eq!(decoded, tx_envelope);
//     }
//
//     #[test]
//     fn test_serde_roundtrip_deposit() {
//         let tx = TxDeposit {
//             gas_limit: u128::MAX,
//             to: TxKind::Call(Address::random()),
//             value: U256::MAX,
//             input: Bytes::new(),
//             source_hash: U256::MAX.into(),
//             from: Address::random(),
//             mint: Some(u128::MAX),
//             is_system_transaction: false,
//         };
//         let tx_envelope = OpTxEnvelope::Deposit(tx);
//
//         let serialized = serde_json::to_string(&tx_envelope).unwrap();
//         let deserialized: OpTxEnvelope = serde_json::from_str(&serialized).unwrap();
//
//         assert_eq!(tx_envelope, deserialized);
//     }
//
//     #[test]
//     fn eip2718_deposit_decode() {
//         // <https://basescan.org/tx/0xc468b38a20375922828c8126912740105125143b9856936085474b2590bbca91>
//         let b = hex!("7ef8f8a0417d134467f4737fcdf2475f0ecdd2a0ed6d87ecffc888ba9f60ee7e3b8ac26a94deaddeaddeaddeaddeaddeaddeaddeaddead00019442000000000000000000000000000000000000158080830f424080b8a4440a5e20000008dd00101c1200000000000000040000000066c352bb000000000139c4f500000000000000000000000000000000000000000000000000000000c0cff1460000000000000000000000000000000000000000000000000000000000000001d4c88f4065ac9671e8b1329b90773e89b5ddff9cf8675b2b5e9c1b28320609930000000000000000000000005050f69a9786f081509234f1a7f4684b5e5b76c9");
//
//         let tx = OpTxEnvelope::decode_2718(&mut b[..].as_ref()).unwrap();
//         let deposit = tx.as_deposit().unwrap();
//         assert!(deposit.mint.is_none());
//     }
// }
