// use std::sync::Arc;

// use alloy_consensus::Transaction;
// use alloy_eips::Encodable2718;
// use alloy_op_evm::{block::receipt_builder::OpReceiptBuilder, OpBlockExecutor};
// use alloy_primitives::{Address, U256};
// use op_alloy_consensus::{OpTxEnvelope, OpTxReceipt};
// use reth::network::types::state;
// use reth::revm::State;
// use reth_evm::Evm;
// use reth_evm::{
//     block::{BlockExecutionError, BlockExecutor, CommitChanges, ExecutableTx},
//     eth::receipt_builder::ReceiptBuilderCtx,
//     op_revm::OpHaltReason,
//     Database, FromRecoveredTx, FromTxWithEncoded, OnStateHook,
// };
// use reth_optimism_chainspec::OpChainSpec;
// use reth_optimism_node::OpRethReceiptBuilder;
// use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
// use reth_primitives::{transaction::SignedTransaction, SealedHeader};
// use reth_provider::{BlockExecutionResult, ProviderError};
// use revm::context::result::ResultAndState;
// use revm::database::BundleState;
// use revm::{
//     context::result::{ExecResultAndState, ExecutionResult},
//     primitives::HashMap,
// };
// use revm_state::{Account, EvmState};
// use tracing::warn;

// pub struct FlashblocksBlockExecutor<'a, DB, E, R>
// where
//     E: Evm<
//             DB = &'a mut State<DB>,
//             Tx: FromRecoveredTx<OpTransactionSigned> + FromTxWithEncoded<OpTransactionSigned>,
//         > + 'a,
//     R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt>,
//     DB: Database + 'a,
// {
//     /// The total flashblocks that have been executed.
//     pub total_flashblocks: u64,
//     /// The index of the current flashblock in the transactions, and receipts.
//     pub current_flashblock_offset: u64,
//     /// Aggregated receipts.
//     pub receipts: Vec<R::Receipt>,
//     /// Latest flashblocks bundle state.
//     pub bundle_prestate: BundleState,
//     /// All executed transactions (unrecovered).
//     pub executed_transactions: Vec<E::Tx>,
//     /// The recovered senders for the executed transactions.
//     pub executed_senders: Vec<Address>,
//     /// All gas used so far
//     pub cumulative_gas_used: u64,
//     /// Estimated DA size
//     pub cumulative_da_bytes_used: u64,
//     /// Tracks fees from executed mempool transactions
//     pub total_fees: U256,
//     /// The inner block executor.
//     /// This is used to execute the block and commit changes.
//     inner: OpBlockExecutor<E, R, OpChainSpec>,
// }

// impl<'a, DB, E, R> BlockExecutor for FlashblocksBlockExecutor<'a, DB, E, R>
// where
//     E: Evm<
//         DB = &'a mut State<DB>,
//         Tx: FromRecoveredTx<OpTxEnvelope> + FromTxWithEncoded<OpTxEnvelope>,
//     >,
//     R: OpReceiptBuilder<Transaction = OpTxEnvelope, Receipt = OpReceipt>,
//     <E as reth_evm::Evm>::Tx: FromRecoveredTx<OpTxEnvelope>,
//     <E as reth_evm::Evm>::Tx: FromTxWithEncoded<OpTxEnvelope>,
//     DB: Database + 'a,
// {
//     type Transaction = OpTransactionSigned;
//     type Receipt = OpReceipt;
//     type Evm = E;

//     fn execute_transaction_with_commit_condition(
//         &mut self,
//         tx: impl ExecutableTx<Self>,
//         f: impl FnOnce(&ExecutionResult<<Self::Evm as Evm>::HaltReason>) -> CommitChanges,
//     ) -> Result<Option<u64>, BlockExecutionError> {
//         let result = self
//             .inner
//             .execute_transaction_with_commit_condition(tx, f)?;
//         Ok(result)
//     }

//     fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
//         self.inner.apply_pre_execution_changes()
//     }

//     fn finish(
//         mut self,
//     ) -> Result<(Self::Evm, BlockExecutionResult<OpReceipt>), BlockExecutionError> {
//         self.receipts.extend_from_slice(&result.receipts);
//         self.cumulative_gas_used = result.gas_used;
//         self.bundle_prestate = self.inner.evm_mut().db_mut().take_bundle();
//         self.total_flashblocks += 1;

//         Ok((evm, result))
//     }

//     fn set_state_hook(&mut self, _hook: Option<Box<dyn OnStateHook>>) {
//         self.inner.set_state_hook(_hook)
//     }

//     fn evm_mut(&mut self) -> &mut Self::Evm {
//         self.inner.evm_mut()
//     }

//     fn evm(&self) -> &Self::Evm {
//         self.inner.evm()
//     }

//     fn execute_transaction(
//         &mut self,
//         tx: impl ExecutableTx<Self>,
//     ) -> Result<u64, BlockExecutionError> {
//         self.execute_transaction_with_result_closure(tx, |result| {
//             self.on_execute_tx(result, &tx);
//             CommitChanges::Yes
//         })
//     }

//     fn apply_post_execution_changes(
//         self,
//     ) -> Result<BlockExecutionResult<Self::Receipt>, BlockExecutionError>
//     where
//         Self: Sized,
//     {
//         self.finish().map(|(_, result)| result)
//     }

//     fn with_state_hook(mut self, hook: Option<Box<dyn OnStateHook>>) -> Self
//     where
//         Self: Sized,
//     {
//         self.set_state_hook(hook);
//         self
//     }

//     fn execute_block(
//         mut self,
//         transactions: impl IntoIterator<Item = impl ExecutableTx<Self>>,
//     ) -> Result<BlockExecutionResult<Self::Receipt>, BlockExecutionError>
//     where
//         Self: Sized,
//     {
//         self.apply_pre_execution_changes()?;

//         for tx in transactions {
//             self.execute_transaction(tx)?;
//         }

//         self.apply_post_execution_changes()
//     }
// }

// impl<'db, DB, E, R> FlashblocksBlockExecutor<'db, DB, E, R>
// where
//     E: Evm<DB = &'db mut State<DB>, Tx: R::Transaction>,
//     R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt>,
//     DB: Database + 'db,
// {
//     pub fn new(inner: OpBlockExecutor<E, R, OpChainSpec>) -> Self {
//         Self {
//             total_flashblocks: 0,
//             executed_transactions: Vec::new(),
//             executed_senders: Vec::new(),
//             current_flashblock_offset: 0,
//             bundle_prestate: BundleState::default(),
//             cumulative_gas_used: 0,
//             cumulative_da_bytes_used: 0,
//             total_fees: U256::ZERO,
//             receipts: Vec::new(),
//             inner,
//         }
//     }

//     pub fn on_execution_tx(
//         &'db mut self,
//         result: &ExecutionResult,
//         tx: &OpTransactionSigned,
//     ) -> Result<(), BlockExecutionError> {
//         if let ExecutionResult::Success {
//             reason,
//             gas_used,
//             gas_refunded,
//             logs,
//             output,
//         } = result
//         {
//             self.cumulative_da_bytes_used += tx.network_len() as u64;
//             let miner_fee = tx
//                 .effective_tip_per_gas(self.inner.evm().block().basefee)
//                 .expect("fee is always valid; execution succeeded");
//             self.total_fees += U256::from(miner_fee) * U256::from(gas_used.to::<u64>());

//             let recovered = tx
//                 .try_recover_unchecked()
//                 .expect("transaction should be recoverable");

//             self.executed_transactions.push(tx.clone());
//             self.executed_senders.push(recovered);
//         } else {
//             warn!("Transaction execution failed: {:?}", result);
//         }

//         Ok(())
//     }

//     pub fn on_execute_block(
//         &mut self,
//         result: &BlockExecutionResult<R::Receipt>,
//     ) -> Result<(), BlockExecutionError> {
//         self.receipts.extend_from_slice(&result.receipts);
//         self.current_flashblock_offset + self.receipts.len() as u64;
//         self.total_flashblocks += 1;

//         self.bundle_prestate = self.inner.evm_mut().db_mut().take_bundle();
//         self.cumulative_gas_used = result.gas_used;

//         Ok(())
//     }
// }
