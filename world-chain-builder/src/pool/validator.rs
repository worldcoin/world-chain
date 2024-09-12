//! World Chain transaction pool types
use std::sync::Arc;

use reth_db::{Database, DatabaseEnv};
use reth_node_optimism::txpool::OpTransactionValidator;
use reth_primitives::SealedBlock;
use reth_provider::{BlockReaderIdExt, StateProviderFactory};
use reth_transaction_pool::{
    CoinbaseTipOrdering, EthPooledTransaction, EthTransactionValidator, Pool, TransactionOrigin,
    TransactionValidationOutcome, TransactionValidationTaskExecutor, TransactionValidator,
};

use crate::nullifier::NullifierTable;

use super::tx::WorldChainPooledTransaction;

/// Type alias for World Chain transaction pool
pub type WorldChainTransactionPool<Client, S> = Pool<
    TransactionValidationTaskExecutor<
        WorldChainTransactionValidator<Client, WorldChainPooledTransaction>,
    >,
    CoinbaseTipOrdering<WorldChainPooledTransaction>,
    S,
>;

/// Validator for World Chain transactions.
#[derive(Debug, Clone)]
pub struct WorldChainTransactionValidator<Client, Tx> {
    inner: OpTransactionValidator<Client, Tx>,
    database_env: Arc<DatabaseEnv>,
    tmp_workaround: EthTransactionValidator<Client, Tx>,
}

impl<Client, Tx> WorldChainTransactionValidator<Client, Tx>
where
    Client: StateProviderFactory + BlockReaderIdExt,
    //    Tx: EthPoolTransaction,
{
    /// Create a new [`WorldChainTransactionValidator`].
    pub fn new(
        inner: OpTransactionValidator<Client, Tx>,
        database_env: Arc<DatabaseEnv>,
        tmp_workaround: EthTransactionValidator<Client, Tx>,
    ) -> Self {
        Self {
            inner,
            database_env,
            tmp_workaround,
        }
    }
}

impl<Client> TransactionValidator
    for WorldChainTransactionValidator<Client, WorldChainPooledTransaction>
where
    Client: StateProviderFactory + BlockReaderIdExt,
    // Tx: EthPoolTransaction,
{
    type Transaction = WorldChainPooledTransaction;

    async fn validate_transaction(
        &self,
        origin: TransactionOrigin,
        transaction: Self::Transaction,
    ) -> TransactionValidationOutcome<Self::Transaction> {
        let tx = self.database_env.tx_mut().unwrap();
        tx.get_dbi::<NullifierTable>().unwrap();
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

// #[cfg(test)]
// mod tests {
//     use reth_chainspec::MAINNET;
//     use reth_node_optimism::txpool::OpTransactionValidator;
//     use reth_primitives::{
//         Signature, Transaction, TransactionSigned, TransactionSignedEcRecovered, TxDeposit, TxKind,
//         U256,
//     };
//     use reth_provider::test_utils::MockEthProvider;
//     use reth_transaction_pool::TransactionValidator as _;
//     use reth_transaction_pool::{
//         blobstore::InMemoryBlobStore, validate::EthTransactionValidatorBuilder,
//         EthPooledTransaction, TransactionOrigin, TransactionValidationOutcome,
//     };
//
//     use crate::txpool::WorldChainTransactionValidator;
//
//     #[tokio::test]
//     async fn validate_optimism_transaction() {
//         let client = MockEthProvider::default();
//         let validator = EthTransactionValidatorBuilder::new(MAINNET.clone())
//             .no_shanghai()
//             .no_cancun()
//             .build(client, InMemoryBlobStore::default());
//         let op = OpTransactionValidator::new(validator);
//         let validator = WorldChainTransactionValidator::new(op);
//
//         let origin = TransactionOrigin::External;
//         let signer = Default::default();
//         let deposit_tx = Transaction::Deposit(TxDeposit {
//             source_hash: Default::default(),
//             from: signer,
//             to: TxKind::Create,
//             mint: None,
//             value: U256::ZERO,
//             gas_limit: 0,
//             is_system_transaction: false,
//             input: Default::default(),
//         });
//         let signature = Signature::default();
//         let signed_tx = TransactionSigned::from_transaction_and_signature(deposit_tx, signature);
//         let signed_recovered =
//             TransactionSignedEcRecovered::from_signed_transaction(signed_tx, signer);
//         let len = signed_recovered.length_without_header();
//         let pooled_tx = EthPooledTransaction::new(signed_recovered, len);
//         let outcome = validator
//             .validate_transaction(origin, pooled_tx.clone())
//             .await;
//
//         let err = match outcome {
//             TransactionValidationOutcome::Invalid(_, err) => err,
//             _ => panic!("Expected invalid transaction"),
//         };
//         assert_eq!(err.to_string(), "transaction type not supported");
//     }
// }
