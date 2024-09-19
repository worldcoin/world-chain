use alloy_rlp::{bytes, Decodable, Encodable};
use reth_primitives::transaction::TransactionConversionError;
use reth_primitives::{
    PooledTransactionsElement, PooledTransactionsElementEcRecovered, TransactionSigned,
    TransactionSignedEcRecovered,
};
use reth_rpc_eth_types::{EthApiError, EthResult};
use revm_primitives::Bytes;

use crate::pbh::semaphore::SemaphoreProof;

pub struct WorldChainTransactionSignedEcRecovered {
    inner: TransactionSignedEcRecovered,
    semaphore_proof: Option<SemaphoreProof>,
}

pub struct WorldChainPooledTransactionsElement {
    inner: PooledTransactionsElement,
    semaphore_proof: Option<SemaphoreProof>,
}

impl Encodable for WorldChainPooledTransactionsElement {
    /// Encodes an enveloped post EIP-4844 [`PooledTransactionsElement`].
    ///
    /// For legacy transactions, this encodes the transaction as `rlp(tx-data)`.
    ///
    /// For EIP-2718 transactions, this encodes the transaction as `rlp(tx_type || rlp(tx-data)))`,
    /// ___including__ the RLP-header for the entire transaction.
    fn encode(&self, out: &mut dyn bytes::BufMut) {
        self.inner.encode(out)
    }

    fn length(&self) -> usize {
        self.inner.length()
    }
}

impl Decodable for WorldChainPooledTransactionsElement {
    /// Decodes an enveloped post EIP-4844 [`PooledTransactionsElement`].
    ///
    /// CAUTION: this expects that `buf` is `rlp(tx_type || rlp(tx-data))`
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let inner = PooledTransactionsElement::decode(buf)?;
        Ok(Self {
            inner,
            semaphore_proof: None,
        })
    }
}

impl TryFrom<TransactionSigned> for WorldChainPooledTransactionsElement {
    type Error = TransactionConversionError;

    fn try_from(tx: TransactionSigned) -> Result<Self, Self::Error> {
        let inner = PooledTransactionsElement::try_from(tx)?;

        Ok(Self {
            inner,
            semaphore_proof: None,
        })
    }
}

pub struct WorldChainPooledTransactionsElementEcRecovered {
    inner: PooledTransactionsElementEcRecovered,
    semaphore_proof: Option<SemaphoreProof>,
}

impl TryFrom<WorldChainTransactionSignedEcRecovered>
    for WorldChainPooledTransactionsElementEcRecovered
{
    type Error = TransactionConversionError;

    fn try_from(tx: WorldChainTransactionSignedEcRecovered) -> Result<Self, Self::Error> {
        let inner = tx.inner.try_into()?;
        Ok(Self {
            inner,
            semaphore_proof: tx.semaphore_proof,
        })
    }
}

/// Recovers a [`PooledTransactionsElementEcRecovered`] from an enveloped encoded byte stream.
///
/// See [`PooledTransactionsElement::decode_enveloped`]
pub fn recover_raw_transaction(data: Bytes) -> EthResult<PooledTransactionsElementEcRecovered> {
    if data.is_empty() {
        return Err(EthApiError::EmptyRawTransactionData);
    }

    let transaction = PooledTransactionsElement::decode_enveloped(&mut data.as_ref())
        .map_err(|_| EthApiError::FailedToDecodeSignedTransaction)?;

    transaction
        .try_into_ecrecovered()
        .or(Err(EthApiError::InvalidTransactionSignature))
}
