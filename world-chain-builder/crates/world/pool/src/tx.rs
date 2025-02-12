use std::sync::Arc;

use alloy_consensus::{BlobTransactionSidecar, BlobTransactionValidationError};
use alloy_eips::Typed2718;
use alloy_primitives::{Bytes, TxHash};
use alloy_rpc_types::erc4337::TransactionConditional;
use reth::transaction_pool::{
    error::{InvalidPoolTransactionError, PoolTransactionError},
    EthBlobTransactionSidecar, EthPoolTransaction, PoolTransaction, TransactionValidationOutcome,
};
use reth_optimism_node::txpool::{conditional::MaybeConditionalTransaction, OpPooledTransaction};
use reth_optimism_primitives::OpTransactionSigned;
use reth_primitives::Recovered;
use reth_primitives_traits::InMemorySize;
use revm_primitives::{
    AccessList, Address, InvalidTransaction, KzgSettings, SignedAuthorization, TxKind, B256, U256,
};
use thiserror::Error;
use world_chain_builder_pbh::payload::PbhValidationError;

#[derive(Debug, Clone)]
pub struct WorldChainPooledTransaction {
    pub inner: OpPooledTransaction,
    pub valid_pbh: bool,
}

pub trait WorldChainPoolTransaction: EthPoolTransaction {
    fn valid_pbh(&self) -> bool;
    fn set_valid_pbh(&mut self);
    fn conditional_options(&self) -> Option<&TransactionConditional>;
}

impl WorldChainPoolTransaction for WorldChainPooledTransaction {
    fn valid_pbh(&self) -> bool {
        self.valid_pbh
    }

    fn conditional_options(&self) -> Option<&TransactionConditional> {
        self.inner.conditional()
    }

    fn set_valid_pbh(&mut self) {
        self.valid_pbh = true;
    }
}

impl Typed2718 for WorldChainPooledTransaction {
    fn ty(&self) -> u8 {
        self.inner.ty()
    }
}

impl alloy_consensus::Transaction for WorldChainPooledTransaction {
    fn chain_id(&self) -> Option<u64> {
        self.inner.chain_id()
    }

    fn nonce(&self) -> u64 {
        self.inner.nonce()
    }

    fn gas_limit(&self) -> u64 {
        self.inner.gas_limit()
    }

    fn gas_price(&self) -> Option<u128> {
        self.inner.gas_price()
    }

    fn max_fee_per_gas(&self) -> u128 {
        self.inner.max_fee_per_gas()
    }

    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        self.inner.max_priority_fee_per_gas()
    }

    fn max_fee_per_blob_gas(&self) -> Option<u128> {
        self.inner.max_fee_per_blob_gas()
    }

    fn priority_fee_or_price(&self) -> u128 {
        self.inner.priority_fee_or_price()
    }

    fn effective_gas_price(&self, base_fee: Option<u64>) -> u128 {
        self.inner.effective_gas_price(base_fee)
    }

    fn is_dynamic_fee(&self) -> bool {
        self.inner.is_dynamic_fee()
    }

    fn kind(&self) -> TxKind {
        self.inner.kind()
    }

    fn is_create(&self) -> bool {
        self.inner.is_create()
    }

    fn value(&self) -> U256 {
        self.inner.value()
    }

    fn input(&self) -> &Bytes {
        self.inner.input()
    }

    fn access_list(&self) -> Option<&AccessList> {
        self.inner.access_list()
    }

    fn blob_versioned_hashes(&self) -> Option<&[B256]> {
        self.inner.blob_versioned_hashes()
    }

    fn authorization_list(&self) -> Option<&[SignedAuthorization]> {
        self.inner.authorization_list()
    }
}

impl EthPoolTransaction for WorldChainPooledTransaction {
    fn take_blob(&mut self) -> EthBlobTransactionSidecar {
        EthBlobTransactionSidecar::None
    }

    fn try_into_pooled_eip4844(
        self,
        sidecar: Arc<BlobTransactionSidecar>,
    ) -> Option<Recovered<Self::Pooled>> {
        self.inner.try_into_pooled_eip4844(sidecar)
    }

    fn try_from_eip4844(
        _tx: Recovered<Self::Consensus>,
        _sidecar: BlobTransactionSidecar,
    ) -> Option<Self> {
        None
    }

    fn validate_blob(
        &self,
        _sidecar: &BlobTransactionSidecar,
        _settings: &KzgSettings,
    ) -> Result<(), BlobTransactionValidationError> {
        Err(BlobTransactionValidationError::NotBlobTransaction(
            self.ty(),
        ))
    }
}

impl InMemorySize for WorldChainPooledTransaction {
    // TODO: double check this
    fn size(&self) -> usize {
        self.inner.size()
    }
}

impl MaybeConditionalTransaction for WorldChainPooledTransaction {
    fn set_conditional(&mut self, conditional: TransactionConditional) {
        self.inner.set_conditional(conditional)
    }

    fn with_conditional(mut self, conditional: TransactionConditional) -> Self
    where
        Self: Sized,
    {
        self.set_conditional(conditional);
        self
    }
}

impl PoolTransaction for WorldChainPooledTransaction {
    type TryFromConsensusError =
        <op_alloy_consensus::OpPooledTransaction as TryFrom<OpTransactionSigned>>::Error;
    type Consensus = OpTransactionSigned;
    type Pooled = op_alloy_consensus::OpPooledTransaction;

    fn clone_into_consensus(&self) -> Recovered<Self::Consensus> {
        self.inner.clone_into_consensus()
    }

    fn into_consensus(self) -> Recovered<Self::Consensus> {
        self.inner.into_consensus()
    }

    fn from_pooled(tx: Recovered<Self::Pooled>) -> Self {
        let inner = OpPooledTransaction::from_pooled(tx);
        Self {
            inner,
            valid_pbh: false,
        }
    }

    fn hash(&self) -> &TxHash {
        self.inner.hash()
    }

    fn sender(&self) -> Address {
        self.inner.sender()
    }

    fn sender_ref(&self) -> &Address {
        self.inner.sender_ref()
    }

    fn cost(&self) -> &U256 {
        self.inner.cost()
    }

    fn encoded_length(&self) -> usize {
        self.inner.encoded_length()
    }
}

#[derive(Debug, Error)]
pub enum WorldChainPoolTransactionError {
    #[error("Conditional Validation Failed: {0}")]
    ConditionalValidationFailed(B256),
    #[error(transparent)]
    InvalidTransaction(#[from] InvalidTransaction),
    #[error(transparent)]
    PbhValidationError(#[from] PbhValidationError),
    #[error("Invalid calldata encoding")]
    InvalidCalldata,
    #[error("Missing PBH Payload")]
    MissingPbhPayload,
    #[error("InvalidSignatureAggregator")]
    InvalidSignatureAggregator,
    #[error("PBH call tracer error")]
    PBHCallTracerError,
}

impl WorldChainPoolTransactionError {
    pub fn to_outcome<T: PoolTransaction>(self, tx: T) -> TransactionValidationOutcome<T> {
        TransactionValidationOutcome::Invalid(tx, self.into())
    }
}

impl From<WorldChainPoolTransactionError> for InvalidPoolTransactionError {
    fn from(val: WorldChainPoolTransactionError) -> Self {
        InvalidPoolTransactionError::Other(Box::new(val))
    }
}

//TODO: double check this?
impl PoolTransactionError for WorldChainPoolTransactionError {
    fn is_bad_transaction(&self) -> bool {
        // TODO: double check if invalid transaction should be penalized, we could also make this a match statement
        // If all errors should not be penalized, we can just return false
        false
    }

    fn as_any(&self) -> &dyn std::any::Any {
        todo!("TODO:")
    }
}

impl From<OpPooledTransaction> for WorldChainPooledTransaction {
    fn from(tx: OpPooledTransaction) -> Self {
        Self {
            inner: tx,
            valid_pbh: false,
        }
    }
}
