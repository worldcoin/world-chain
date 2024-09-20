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

impl WorldChainPooledTransactionsElement {
    /// FIXME: We need to implement the proper decoding
    /// At the end the buffer must point to the start of the inner transaction
    pub fn decode_enveloped(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let inner = PooledTransactionsElement::decode(buf)?;
        Ok(Self {
            inner,
            semaphore_proof: None,
        })
    }

    pub fn try_into_ecrecovered(
        self,
    ) -> Result<WorldChainPooledTransactionsElementEcRecovered, PooledTransactionsElement> {
        let inner = self.inner.try_into_ecrecovered()?;
        Ok(WorldChainPooledTransactionsElementEcRecovered {
            inner,
            semaphore_proof: self.semaphore_proof,
        })
    }
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
    pub inner: PooledTransactionsElementEcRecovered,
    pub semaphore_proof: Option<SemaphoreProof>,
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

/// Recovers the raw transaction by parsing the enveloped transaction data.
/// Returns the recovered World Chain transaction along with data corresponding
/// to the inner transaction.
pub fn recover_raw_transaction(
    data: Bytes,
) -> EthResult<(WorldChainPooledTransactionsElementEcRecovered, Bytes)> {
    if data.is_empty() {
        return Err(EthApiError::EmptyRawTransactionData);
    }

    let transaction = WorldChainPooledTransactionsElement::decode_enveloped(&mut data.as_ref())
        .map_err(|_| EthApiError::FailedToDecodeSignedTransaction)?;

    let ecrecovered = transaction
        .try_into_ecrecovered()
        .or(Err(EthApiError::InvalidTransactionSignature))?;

    Ok((ecrecovered, data))
}
