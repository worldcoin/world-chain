//! World Chain transaction pool types
use reth_node_optimism::txpool::OpTransactionValidator;
use reth_primitives::SealedBlock;
use reth_provider::{BlockReaderIdExt, StateProviderFactory};
use reth_transaction_pool::{
    CoinbaseTipOrdering, EthPoolTransaction, EthPooledTransaction, Pool, TransactionOrigin,
    TransactionValidationOutcome, TransactionValidationTaskExecutor, TransactionValidator,
};

/// Type alias for World Chain transaction pool
pub type WorldChainTransactionPool<Client, S> = Pool<
    TransactionValidationTaskExecutor<OpTransactionValidator<Client, EthPooledTransaction>>,
    CoinbaseTipOrdering<EthPooledTransaction>,
    S,
>;

/// Validator for World Chain transactions.
#[derive(Debug, Clone)]
pub struct WorldChainTransactionValidator<Client, Tx> {
    inner: OpTransactionValidator<Client, Tx>,
}

impl<Client, Tx> WorldChainTransactionValidator<Client, Tx>
where
    Client: StateProviderFactory + BlockReaderIdExt,
    Tx: EthPoolTransaction,
{
    /// Create a new [`WorldChainTransactionValidator`].
    pub fn new(inner: OpTransactionValidator<Client, Tx>) -> Self {
        Self { inner }
    }
}

impl<Client, Tx> TransactionValidator for WorldChainTransactionValidator<Client, Tx>
where
    Client: StateProviderFactory + BlockReaderIdExt,
    Tx: EthPoolTransaction,
{
    type Transaction = Tx;

    async fn validate_transaction(
        &self,
        origin: TransactionOrigin,
        transaction: Self::Transaction,
    ) -> TransactionValidationOutcome<Self::Transaction> {
        self.inner.validate_transaction(origin, transaction).await
    }

    async fn validate_transactions(
        &self,
        transactions: Vec<(TransactionOrigin, Self::Transaction)>,
    ) -> Vec<TransactionValidationOutcome<Self::Transaction>> {
        self.inner.validate_transactions(transactions).await
    }

    fn on_new_head_block(&self, new_tip_block: &SealedBlock) {
        self.inner.on_new_head_block(new_tip_block)
    }
}

#[cfg(test)]
mod tests {
    use crate::txpool::OpTransactionValidator;
    use reth_chainspec::MAINNET;
    use reth_primitives::{
        Signature, Transaction, TransactionSigned, TransactionSignedEcRecovered, TxDeposit, TxKind,
        U256,
    };
    use reth_provider::test_utils::MockEthProvider;
    use reth_transaction_pool::{
        blobstore::InMemoryBlobStore, validate::EthTransactionValidatorBuilder,
        EthPooledTransaction, TransactionOrigin, TransactionValidationOutcome,
    };

    #[test]
    fn validate_optimism_transaction() {
        let client = MockEthProvider::default();
        let validator = EthTransactionValidatorBuilder::new(MAINNET.clone())
            .no_shanghai()
            .no_cancun()
            .build(client, InMemoryBlobStore::default());
        let validator = OpTransactionValidator::new(validator);

        let origin = TransactionOrigin::External;
        let signer = Default::default();
        let deposit_tx = Transaction::Deposit(TxDeposit {
            source_hash: Default::default(),
            from: signer,
            to: TxKind::Create,
            mint: None,
            value: U256::ZERO,
            gas_limit: 0,
            is_system_transaction: false,
            input: Default::default(),
        });
        let signature = Signature::default();
        let signed_tx = TransactionSigned::from_transaction_and_signature(deposit_tx, signature);
        let signed_recovered =
            TransactionSignedEcRecovered::from_signed_transaction(signed_tx, signer);
        let len = signed_recovered.length_without_header();
        let pooled_tx = EthPooledTransaction::new(signed_recovered, len);
        let outcome = validator.validate_one(origin, pooled_tx);

        let err = match outcome {
            TransactionValidationOutcome::Invalid(_, err) => err,
            _ => panic!("Expected invalid transaction"),
        };
        assert_eq!(err.to_string(), "transaction type not supported");
    }
}
