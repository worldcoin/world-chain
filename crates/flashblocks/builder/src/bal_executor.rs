//! Types and utilities for BAL (Block Access List) execution and validation.

use alloy_consensus::{BlockHeader, transaction::TxHashRef};
use alloy_op_evm::block::receipt_builder::OpReceiptBuilder;
use alloy_primitives::{FixedBytes, U256};
use flashblocks_primitives::access_list::FlashblockAccessList;
use op_alloy_consensus::OpReceipt;
use reth_evm::block::BlockExecutionError;
use reth_optimism_evm::OpRethReceiptBuilder;
use reth_optimism_node::OpBuiltPayload;
use reth_optimism_primitives::OpTransactionSigned;
use reth_payload_primitives::BuiltPayload;
use reth_primitives::Recovered;
use revm::database::BundleState;

use crate::access_list::BlockAccessIndex;

/// Errors that can occur during BAL validation.
#[derive(thiserror::Error, Debug, serde::Serialize)]
pub enum BalValidationError {
    #[error("BAL Hash Mismatch: expected {expected:?}, got {got:?}")]
    BalHashMismatch {
        expected: FixedBytes<32>,
        got: FixedBytes<32>,
        expected_bal: FlashblockAccessList,
        got_bal: FlashblockAccessList,
    },
    #[error("Block Hash Mismatch: expected {expected:?}, got {got:?}")]
    BlockHashMismatch {
        expected: FixedBytes<32>,
        got: FixedBytes<32>,
    },
    #[error("State Root Mismatch: expected {expected:?}, got {got:?}")]
    StateRootMismatch {
        expected: FixedBytes<32>,
        got: FixedBytes<32>,
        bundle_state: BundleState,
    },
    #[error("Receipts Root Mismatch: expected {expected:?}, got: {got:?}")]
    ReceiptsRootMismatch {
        expected: FixedBytes<32>,
        got: FixedBytes<32>,
    },
    #[error("Missing access list data")]
    MissingAccessListData,
}

impl BalValidationError {
    pub fn boxed(self) -> Box<Self> {
        Box::new(self)
    }
}

/// Errors that can occur during BAL execution.
#[derive(thiserror::Error, Debug, serde::Serialize)]
pub enum BalExecutorError {
    #[error(transparent)]
    #[serde(skip_serializing)]
    BlockExecutionError(#[from] BlockExecutionError),
    #[error("Missing executed block in built payload")]
    MissingExecutedBlock,
    #[error(transparent)]
    BalValidationError(#[from] Box<BalValidationError>),
    #[error("Internal Error: {0}")]
    #[serde(skip_serializing)]
    Other(#[from] Box<dyn core::error::Error + Send + Sync>),
}

impl BalExecutorError {
    pub fn other<E: core::error::Error + Send + Sync + 'static>(err: E) -> Self {
        BalExecutorError::Other(Box::new(err))
    }
}

/// [`CommittedState`] holds all relevant information about the intra-block state commitment
/// for which we are executing on top of.
///
/// This is used when building flashblocks incrementally, where each flashblock builds
/// on top of the state from previous committed transactions.
#[derive(Default, Debug, Clone)]
pub struct CommittedState<R: OpReceiptBuilder + Default = OpRethReceiptBuilder> {
    /// The total gas used in previous committed transactions.
    pub gas_used: u64,
    /// The total fees accumulated in previous committed transactions.
    pub fees: U256,
    /// The bundle state accumulated so far from the State Transitions
    pub bundle: BundleState,
    /// Ordered receipts of previous committed transactions.
    pub receipts: Vec<(BlockAccessIndex, R::Receipt)>,
    /// Ordered transactions which have been executed
    pub transactions: Vec<(BlockAccessIndex, Recovered<R::Transaction>)>,
}

impl<R> CommittedState<R>
where
    R: OpReceiptBuilder + Default,
{
    /// Iterator over committed transactions
    pub fn transactions_iter(&self) -> impl Iterator<Item = &'_ Recovered<R::Transaction>> + '_ {
        self.transactions.iter().map(|(_, tx)| tx)
    }

    /// Iterator over committed transaction hashes
    pub fn transaction_hashes_iter(&self) -> impl Iterator<Item = FixedBytes<32>> + '_
    where
        R::Transaction: Clone + TxHashRef,
    {
        self.transactions
            .iter()
            .map(|(_, tx)| tx.tx_hash())
            .copied()
    }

    /// Iterator over committed receipts
    pub fn receipts_iter(&self) -> impl Iterator<Item = &'_ R::Receipt> + '_ {
        self.receipts.iter().map(|(_, r)| r)
    }
}

impl<R> TryFrom<Option<&OpBuiltPayload>> for CommittedState<R>
where
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt> + Default,
{
    type Error = BalExecutorError;

    fn try_from(value: Option<&OpBuiltPayload>) -> Result<Self, Self::Error> {
        if let Some(value) = value {
            let executed_block = value
                .executed_block()
                .ok_or(BalExecutorError::MissingExecutedBlock)?;

            let gas_used = executed_block.recovered_block.gas_used();

            let bundle = value
                .executed_block()
                .ok_or(BalExecutorError::MissingExecutedBlock)?
                .execution_output
                .bundle
                .clone();

            let fees = value.fees();

            let transactions: Vec<_> = executed_block
                .recovered_block
                .clone_transactions_recovered()
                .enumerate()
                .map(|(index, tx)| (index as BlockAccessIndex, tx))
                .collect();

            let receipts: Vec<_> = executed_block
                .execution_output
                .receipts()
                .iter()
                .flatten()
                .cloned()
                .enumerate()
                .map(|(index, r)| (index as BlockAccessIndex, r))
                .collect();

            Ok(Self {
                transactions,
                receipts,
                gas_used,
                fees,
                bundle,
            })
        } else {
            Ok(Self {
                transactions: vec![],
                receipts: vec![],
                gas_used: 0,
                fees: U256::ZERO,
                bundle: BundleState::default(),
            })
        }
    }
}
