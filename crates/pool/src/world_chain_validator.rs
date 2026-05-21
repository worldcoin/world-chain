//! This file will replace the validator.rs file once I create the WorldChainPrimitives type and
//! wire it everywhere it's needed. This also makes it possible to keep our existent codebase
//! unaltered so that we don't change our current functionalities while developing wip1001 features.

use crate::{
    world_chain_account_manager::WorldChainAccountManagerReader,
    world_chain_tx::{WorldChainPoolTransaction, WorldChainPoolTransactionError},
};
use reth_evm::ConfigureEvm;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::txpool::OpTransactionValidator;
use reth_primitives_traits::{
    BlockTy, GotExpected, SealedBlock, TxTy, transaction::error::InvalidTransactionError,
};
use reth_provider::{BlockReaderIdExt, ChainSpecProvider, StateProviderFactory};
use reth_transaction_pool::{
    TransactionOrigin, TransactionValidationOutcome, TransactionValidator,
};
use revm_primitives::U256;
use world_chain_primitives::transaction::WIP_1001_TX_TYPE;

/// The min validation failure fee that world chain account MUST have for
/// a WIP-1001 tx to be considered valid and inserted into the mempool.
// TODO: change this value to an appropriate one!!!
pub const MIN_VALIDATION_FAILURE_FEE: U256 = U256::from_limbs([0xFFFF, 0, 0, 0]);

/// Validator for World Chain transactions.
#[derive(Debug, Clone)]
pub struct WorldChainTransactionValidator<Client, Tx, Evm>
where
    Client: StateProviderFactory + BlockReaderIdExt,
{
    /// The inner transaction validator.
    inner: OpTransactionValidator<Client, Tx, Evm>,
}

impl<Client, Tx, Evm> WorldChainTransactionValidator<Client, Tx, Evm>
where
    Client: ChainSpecProvider<ChainSpec: OpHardforks>
        + StateProviderFactory
        + BlockReaderIdExt<Block = BlockTy<Evm::Primitives>>,
    Tx: WorldChainPoolTransaction<Consensus = TxTy<Evm::Primitives>>,
    Evm: ConfigureEvm,
{
    /// Create a new [`WorldChainTransactionValidator`].
    pub fn new(inner: OpTransactionValidator<Client, Tx, Evm>) -> Self {
        Self { inner }
    }

    /// Get a reference to the inner transaction validator.
    pub fn inner(&self) -> &OpTransactionValidator<Client, Tx, Evm> {
        &self.inner
    }

    pub async fn validate_wip1001(
        &self,
        _origin: TransactionOrigin,
        tx: Tx,
    ) -> TransactionValidationOutcome<Tx> {
        let state = match self.inner.client().latest() {
            Ok(new_state) => Box::new(new_state),
            Err(err) => return TransactionValidationOutcome::Error(*tx.hash(), Box::new(err)),
        };
        // check whether world chain account exists
        // TODO: it's better to call WorldChainAccountManager.getKeyRingHash(account)
        // and ensure it's not zero, therefore we're sure that account is a valid and esistent
        // world chain account.
        let world_chain_account_addr = tx.sender();
        let world_chain_account = match state.basic_account(&world_chain_account_addr) {
            Ok(maybe_account) => match maybe_account {
                Some(account) => account,
                None => {
                    return WorldChainPoolTransactionError::WorldChainAccountDoesNotExist(
                        world_chain_account_addr,
                    )
                    .to_outcome(tx);
                }
            },
            Err(err) => return TransactionValidationOutcome::Error(*tx.hash(), Box::new(err)),
        };
        // world chain account balance must cover `MIN_VALIDATION_FAILURE_FEE`
        if world_chain_account.balance < MIN_VALIDATION_FAILURE_FEE {
            return TransactionValidationOutcome::Invalid(
                tx,
                InvalidTransactionError::InsufficientFunds(
                    GotExpected {
                        got: world_chain_account.balance,
                        expected: MIN_VALIDATION_FAILURE_FEE,
                    }
                    .into(),
                )
                .into(),
            );
        }
        // tx nonce should be greater or equal to the account.transactionNonce
        let tx_nonce = tx.nonce();
        let account_manager = WorldChainAccountManagerReader::new(&*state);
        let account_tx_nonce = match account_manager.transaction_nonce(world_chain_account_addr) {
            Ok(nonce) => nonce,
            Err(err) => return TransactionValidationOutcome::Error(*tx.hash(), Box::new(err)),
        };
        if tx_nonce < account_tx_nonce {
            return WorldChainPoolTransactionError::StaleTransactionNonce {
                got: tx_nonce,
                expected: account_tx_nonce,
            }
            .to_outcome(tx);
        }
        todo!()
    }
}

impl<Client, Tx, Evm> TransactionValidator for WorldChainTransactionValidator<Client, Tx, Evm>
where
    Client: ChainSpecProvider<ChainSpec: OpHardforks>
        + StateProviderFactory
        + BlockReaderIdExt<Block = BlockTy<Evm::Primitives>>,
    Tx: WorldChainPoolTransaction<Consensus = TxTy<Evm::Primitives>>,
    Evm: ConfigureEvm,
{
    type Transaction = Tx;

    type Block = BlockTy<Evm::Primitives>;

    async fn validate_transaction(
        &self,
        origin: TransactionOrigin,
        transaction: Self::Transaction,
    ) -> TransactionValidationOutcome<Self::Transaction> {
        if transaction.ty() == WIP_1001_TX_TYPE {
            // Note: this line is currently unreachable because we're still using legacy
            // OpPooledTransaction that doesn't have the wip1001 tx type, therefore
            // wip1001 txs would fail at the decoding phase, never reaching this point.
            // See: https://vscode.dev/github/worldcoin/world-chain/blob/ale/wip1001-mempool-validation/crates/pool/src/tx.rs#L232
            return self.validate_wip1001(origin, transaction.clone()).await;
        }
        return self.inner.validate_one(origin, transaction.clone()).await;
    }

    fn on_new_head_block(&self, new_tip_block: &SealedBlock<Self::Block>) {
        self.inner.on_new_head_block(new_tip_block);
    }
}
