use reth_primitives::{
    PooledTransactionsElementEcRecovered, TransactionSignedEcRecovered, TxDeposit, TxKind, U256,
};
use reth_transaction_pool::{EthPooledTransaction, PoolTransaction};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldChainPooledTransaction {
    pub inner: EthPooledTransaction,
    pub semaphore_proof: Option<Vec<u8>>,
}

// pub struct WorldChainTransactionSignedEcRecovered {
//     pub inner: TransactionSignedEcRecovered,
// }

impl From<WorldChainPooledTransaction> for TransactionSignedEcRecovered {
    fn from(tx: WorldChainPooledTransaction) -> Self {
        tx.inner.into_consensus()
    }
}

impl From<TransactionSignedEcRecovered> for WorldChainPooledTransaction {
    fn from(tx: TransactionSignedEcRecovered) -> Self {
        todo!()
        // Self {
        //     inner: EthPooledTransaction::from_pooled(tx),
        //     semaphore_proof: None,
        // }
    }
}

impl From<WorldChainPooledTransaction> for PooledTransactionsElementEcRecovered {
    fn from(tx: WorldChainPooledTransaction) -> Self {
        // tx.inner.into_consensus()
        todo!()
    }
}

impl From<PooledTransactionsElementEcRecovered> for WorldChainPooledTransaction {
    fn from(tx: PooledTransactionsElementEcRecovered) -> Self {
        todo!()
        // Self {
        //     inner: EthPooledTransaction::from_pooled(tx),
        //     semaphore_proof: None,
        // }
    }
}

impl PoolTransaction for WorldChainPooledTransaction {
    type TryFromConsensusError = <EthPooledTransaction as PoolTransaction>::TryFromConsensusError;

    type Consensus = TransactionSignedEcRecovered;

    type Pooled = <EthPooledTransaction as PoolTransaction>::Pooled;

    fn try_from_consensus(tx: Self::Consensus) -> Result<Self, Self::TryFromConsensusError> {
        EthPooledTransaction::try_from_consensus(tx).map(|inner| Self {
            inner,
            semaphore_proof: None,
        })
    }

    fn into_consensus(self) -> Self::Consensus {
        self.inner.into_consensus()
    }

    fn from_pooled(pooled: Self::Pooled) -> Self {
        Self {
            inner: EthPooledTransaction::from_pooled(pooled),
            semaphore_proof: None,
        }
    }

    fn hash(&self) -> &reth_primitives::TxHash {
        self.inner.hash()
    }

    fn sender(&self) -> reth_primitives::Address {
        self.inner.sender()
    }

    fn nonce(&self) -> u64 {
        self.inner.nonce()
    }

    fn cost(&self) -> U256 {
        self.inner.cost()
    }

    fn gas_limit(&self) -> u64 {
        self.inner.gas_limit()
    }

    fn max_fee_per_gas(&self) -> u128 {
        self.inner.max_fee_per_gas()
    }

    fn access_list(&self) -> Option<&reth_primitives::AccessList> {
        self.inner.access_list()
    }

    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        self.inner.max_priority_fee_per_gas()
    }

    fn max_fee_per_blob_gas(&self) -> Option<u128> {
        self.inner.max_fee_per_blob_gas()
    }

    fn effective_tip_per_gas(&self, base_fee: u64) -> Option<u128> {
        self.inner.effective_tip_per_gas(base_fee)
    }

    fn priority_fee_or_price(&self) -> u128 {
        self.inner.priority_fee_or_price()
    }

    fn kind(&self) -> TxKind {
        self.inner.kind()
    }

    fn input(&self) -> &[u8] {
        self.inner.input()
    }

    fn size(&self) -> usize {
        self.inner.size()
    }

    fn tx_type(&self) -> u8 {
        self.inner.tx_type()
    }

    fn encoded_length(&self) -> usize {
        self.inner.encoded_length()
    }

    fn chain_id(&self) -> Option<u64> {
        self.inner.chain_id()
    }
}

// #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
// pub struct VerifiedTx {
//     pub signed_transaction: TransactionSigned,
//     pub proof: Vec<u8>,
// }

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