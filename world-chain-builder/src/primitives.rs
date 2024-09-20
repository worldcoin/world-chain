use alloy_rlp::Decodable;
use reth_primitives::transaction::TransactionConversionError;
use reth_primitives::{
    PooledTransactionsElement, PooledTransactionsElementEcRecovered, TransactionSigned,
    TransactionSignedEcRecovered,
};
use reth_rpc_eth_types::{EthApiError, EthResult};
use revm_primitives::Bytes;
use tracing::warn;

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
    pub fn decode_enveloped(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let inner = PooledTransactionsElement::decode_enveloped(buf)?;
        let semaphore_proof = match SemaphoreProof::decode(buf) {
            Ok(res) => Some(res),
            Err(e) => {
                warn!("Failed to decode semaphore proof: {:?}", e);
                None
            }
        };

        Ok(Self {
            inner,
            semaphore_proof,
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
