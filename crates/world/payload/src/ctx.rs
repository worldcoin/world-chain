use alloy_consensus::{SignableTransaction, Transaction};
use alloy_eips::Typed2718;
use alloy_network::{TransactionBuilder, TxSignerSync};
use alloy_rlp::Encodable;
use alloy_signer_local::PrivateKeySigner;
use eyre::eyre::eyre;
use flashblocks::payload_builder_ctx::PayloadBuilderCtx;
use op_alloy_rpc_types::OpTransactionRequest;
use op_revm::OpContext;
use reth::api::PayloadBuilderError;
use reth::payload::PayloadBuilderAttributes;
use reth::revm::State;
use reth::transaction_pool::{BestTransactionsAttributes, TransactionPool};
use reth_evm::block::{BlockExecutionError, BlockValidationError};
use reth_evm::execute::{BlockBuilder, BlockExecutor};
use reth_evm::Evm;
use reth_evm::{ConfigureEvm, Database};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::txpool::interop::MaybeInteropTransaction;
use reth_optimism_node::{
    OpEvm, OpEvmConfig, OpNextBlockEnvAttributes, OpPayloadBuilderAttributes,
};
use reth_optimism_payload_builder::builder::{ExecutionInfo, OpPayloadBuilderCtx};
use reth_optimism_primitives::{OpPrimitives, OpTransactionSigned};
use reth_payload_util::PayloadTransactions;
use reth_primitives::{Block, Recovered, SealedHeader, TxTy};
use reth_primitives_traits::SignedTransaction;
use reth_provider::{BlockReaderIdExt, ChainSpecProvider, StateProviderFactory};
use reth_transaction_pool::PoolTransaction;
use revm::inspector::NoOpInspector;
use revm::{DatabaseCommit, Inspector};
use revm_primitives::{Address, U256};
use semaphore_rs::Field;
use std::collections::HashSet;
use tracing::{error, trace};
use world_chain_builder_pool::bindings::IPBHEntryPoint::spendNullifierHashesCall;
use world_chain_builder_pool::tx::WorldChainPoolTransaction;
use world_chain_builder_rpc::transactions::validate_conditional_options;

/// Container type that holds all necessities to build a new payload.
#[derive(Debug)]
pub struct WorldChainPayloadBuilderCtx<Client, Pool> {
    // TODO: Make Evm and ChainSpec generic here?
    pub inner: OpPayloadBuilderCtx<OpEvmConfig, OpChainSpec>,
    pub verified_blockspace_capacity: u8,
    pub pbh_entry_point: Address,
    pub pbh_signature_aggregator: Address,
    pub client: Client,
    pub builder_private_key: PrivateKeySigner,
    pub pool: Pool,
}

impl<Client, Pool> WorldChainPayloadBuilderCtx<Client, Pool>
where
    Client: StateProviderFactory
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
        + ChainSpecProvider<ChainSpec = OpChainSpec>
        + Clone,
    Pool: TransactionPool<Transaction: WorldChainPoolTransaction<Consensus = OpTransactionSigned>>,
{
    /// Executes the given best transactions and updates the execution info.
    ///
    /// Returns `Ok(Some(())` if the job was cancelled.
    pub fn execute_best_transactions_inner<Txs, DB, Builder>(
        &self,
        info: &mut ExecutionInfo,
        builder: &mut Builder,
        mut best_txs: Txs,
    ) -> Result<Option<()>, PayloadBuilderError>
    where
        DB: Database + DatabaseCommit,
        Builder: BlockBuilder<
            Primitives = OpPrimitives,
            Executor: BlockExecutor<Evm = OpEvm<DB, NoOpInspector>>,
        >,
        Txs: PayloadTransactions<
            Transaction: WorldChainPoolTransaction<Consensus = OpTransactionSigned>,
        >,
    {
        let mut block_gas_limit = builder.evm_mut().block().gas_limit;
        let block_da_limit = self.inner.da_config.max_da_block_size();
        let tx_da_limit = self.inner.da_config.max_da_tx_size();
        let base_fee = builder.evm_mut().block().basefee;

        let mut invalid_txs = vec![];
        let verified_gas_limit = (self.verified_blockspace_capacity as u64 * block_gas_limit) / 100;

        let mut spent_nullifier_hashes = HashSet::new();
        while let Some(pooled_tx) = best_txs.next(()) {
            let tx = pooled_tx.clone().into_consensus();
            if info.is_tx_over_limits(tx.inner(), block_gas_limit, tx_da_limit, block_da_limit) {
                // we can't fit this transaction into the block, so we need to mark it as
                // invalid which also removes all dependent transaction from
                // the iterator before we can continue
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            if let Some(conditional_options) = pooled_tx.conditional_options() {
                if validate_conditional_options(conditional_options, &self.client).is_err() {
                    best_txs.mark_invalid(tx.signer(), tx.nonce());
                    invalid_txs.push(*pooled_tx.hash());
                    continue;
                }
            }

            // ensure we still have capacity for this transaction
            if info.cumulative_gas_used + tx.gas_limit() > block_gas_limit {
                // we can't fit this transaction into the block, so we need to mark it as
                // invalid which also removes all dependent transaction from
                // the iterator before we can continue
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            // A sequencer's block should never contain blob or deposit transactions from the pool.
            if tx.is_eip4844() || tx.is_deposit() {
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            // check if the job was cancelled, if so we can exit early
            if self.inner.cancel.is_cancelled() {
                return Ok(Some(()));
            }

            // If the transaction is verified, check if it can be added within the verified gas limit
            if let Some(payloads) = pooled_tx.pbh_payload() {
                if info.cumulative_gas_used + tx.gas_limit() > verified_gas_limit {
                    best_txs.mark_invalid(tx.signer(), tx.nonce());
                    continue;
                }

                if payloads
                    .iter()
                    .any(|payload| !spent_nullifier_hashes.insert(payload.nullifier_hash))
                {
                    best_txs.mark_invalid(tx.signer(), tx.nonce());
                    invalid_txs.push(*pooled_tx.hash());
                    continue;
                }
            }

            let gas_used = match builder.execute_transaction(tx.clone()) {
                Ok(res) => {
                    if let Some(payloads) = pooled_tx.pbh_payload() {
                        if spent_nullifier_hashes.len() == payloads.len() {
                            block_gas_limit -= FIXED_GAS
                        }

                        block_gas_limit -= COLD_SSTORE_GAS * payloads.len() as u64;
                    }
                    res
                }
                Err(err) => {
                    match err {
                        BlockExecutionError::Validation(BlockValidationError::InvalidTx {
                            error,
                            ..
                        }) => {
                            if error.is_nonce_too_low() {
                                // if the nonce is too low, we can skip this transaction
                                trace!(target: "payload_builder", %error, ?tx, "skipping nonce too low transaction");
                            } else {
                                // if the transaction is invalid, we can skip it and all of its
                                // descendants
                                trace!(target: "payload_builder", %error, ?tx, "skipping invalid transaction and its descendants");
                                best_txs.mark_invalid(tx.signer(), tx.nonce());
                            }

                            continue;
                        }

                        err => {
                            // this is an error that we should treat as fatal for this attempt
                            return Err(PayloadBuilderError::EvmExecutionError(Box::new(err)));
                        }
                    }
                }
            };

            self.commit_changes(info, base_fee, gas_used, tx);
        }

        if !spent_nullifier_hashes.is_empty() {
            let tx = spend_nullifiers_tx(self, builder.evm_mut(), spent_nullifier_hashes).map_err(
                |e| {
                    error!(target: "payload_builder", %e, "failed to build spend nullifiers transaction");
                    PayloadBuilderError::Other(e.into())
                },
            )?;
            let gas_used = builder.execute_transaction(tx.clone()).map_err(
                |e: BlockExecutionError| {
                    error!(target: "payload_builder", %e, "spend nullifiers transaction failed");
                    PayloadBuilderError::evm(e)
                },
            )?;

            self.commit_changes(info, base_fee, gas_used, tx);
        }

        if !invalid_txs.is_empty() {
            self.pool.remove_transactions(invalid_txs);
        }

        Ok(None)
    }

    /// After computing the execution result and state we can commit changes to the database
    fn commit_changes(
        &self,
        info: &mut ExecutionInfo,
        base_fee: u64,
        gas_used: u64,
        tx: Recovered<OpTransactionSigned>,
    ) {
        // add gas used by the transaction to cumulative gas used, before creating the
        // receipt
        info.cumulative_gas_used += gas_used;
        info.cumulative_da_bytes_used += tx.length() as u64;

        // update add to total fees
        let miner_fee = tx
            .effective_tip_per_gas(base_fee)
            .expect("fee is always valid; execution succeeded");
        info.total_fees += U256::from(miner_fee) * U256::from(gas_used);
    }

    /// Prepares [`BlockBuilder`] for the payload. This will configure the underlying EVM with [`PBHCallTracer`],
    /// but disable it by default. It will get enabled by [`WorldChainPayloadBuilderCtx::execute_best_transactions`].
    fn block_builder<'a, DB: Database>(
        &'a self,
        db: &'a mut State<DB>,
    ) -> Result<
        impl BlockBuilder<
            Primitives = OpPrimitives,
            Executor: BlockExecutor<Evm = OpEvm<&'a mut State<DB>, NoOpInspector>>,
        >,
        PayloadBuilderError,
    > {
        // Prepare attributes for next block environment.
        let attributes = OpNextBlockEnvAttributes {
            timestamp: self.inner.attributes().timestamp(),
            suggested_fee_recipient: self.inner.attributes().suggested_fee_recipient(),
            prev_randao: self.inner.attributes().prev_randao(),
            gas_limit: self
                .inner
                .attributes()
                .gas_limit
                .unwrap_or(self.inner.parent().gas_limit),
            parent_beacon_block_root: self.inner.attributes().parent_beacon_block_root(),
            extra_data: self.inner.extra_data()?,
        };

        // Prepare EVM environment.
        let evm_env = self
            .inner
            .evm_config
            .next_evm_env(self.inner.parent(), &attributes)
            .map_err(PayloadBuilderError::other)?;

        // Prepare EVM.
        let evm = self.inner.evm_config.evm_with_env(db, evm_env);

        // Prepare block execution context.
        let execution_ctx = self
            .inner
            .evm_config
            .context_for_next_block(self.inner.parent(), attributes);

        // Prepare block builder.
        Ok(self
            .inner
            .evm_config
            .create_block_builder(evm, self.inner.parent(), execution_ctx))
    }
}

impl<Client, Pool> PayloadBuilderCtx for WorldChainPayloadBuilderCtx<Client, Pool>
where
    Client: Send + Sync,
    Pool: Send + Sync,
{
    type Evm = OpEvmConfig;

    type ChainSpec = OpChainSpec;

    fn evm(&self) -> &Self::Evm {
        self.inner.evm()
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        self.inner.evm_mut()
    }

    fn spec(&self) -> &Self::ChainSpec {
        self.inner.spec()
    }

    fn parent(&self) -> &SealedHeader {
        self.inner.parent()
    }

    fn attributes(
        &self,
    ) -> &OpPayloadBuilderAttributes<reth_primitives::TxTy<<Self::Evm as ConfigureEvm>::Primitives>>
    {
        self.inner.attributes()
    }

    fn extra_data(&self) -> Result<revm_primitives::Bytes, PayloadBuilderError> {
        self.inner.extra_data()
    }

    fn best_transaction_attributes(
        &self,
        block_env: &revm::context::BlockEnv,
    ) -> BestTransactionsAttributes {
        self.inner.best_transaction_attributes(block_env)
    }

    fn payload_id(&self) -> reth::payload::PayloadId {
        self.inner.payload_id()
    }

    fn is_holocene_active(&self) -> bool {
        self.inner.is_holocene_active()
    }

    fn is_better_payload(&self, total_fees: U256) -> bool {
        self.inner.is_better_payload(total_fees)
    }

    fn block_builder<'a, DB>(
        &'a self,
        db: &'a mut State<DB>,
    ) -> Result<
        impl BlockBuilder<Primitives = <Self::Evm as ConfigureEvm>::Primitives> + 'a,
        PayloadBuilderError,
    >
    where
        DB: revm::Database,
        DB::Error: Send + Sync + 'static,
        DB: reth::revm::Database,
    {
        self.inner.block_builder(db)
    }

    fn execute_sequencer_transactions(
        &self,
        builder: &mut impl BlockBuilder<Primitives = <Self::Evm as ConfigureEvm>::Primitives>,
    ) -> Result<ExecutionInfo, PayloadBuilderError> {
        self.inner.execute_sequencer_transactions(builder)
    }

    fn execute_best_transactions<Builder, Txs>(
        &self,
        info: &mut ExecutionInfo,
        builder: &mut Builder,
        best_txs: Txs,
        _gas_limit: u64,
    ) -> Result<Option<()>, PayloadBuilderError>
    where
        Txs: PayloadTransactions<
            Transaction: PoolTransaction<
                Consensus = TxTy<<Self::Evm as ConfigureEvm>::Primitives>,
            > + MaybeInteropTransaction,
        >,
        Builder: BlockBuilder<
            Primitives = <Self::Evm as ConfigureEvm>::Primitives,
            Executor: BlockExecutor,
        >,
    {
        // TODO: Implement PBH functionality & handle gas limit
        // self.execute_best_transactions_inner(...)
        self.inner
            .execute_best_transactions(info, builder, best_txs)
    }
}

pub const COLD_SSTORE_GAS: u64 = 20000;
pub const FIXED_GAS: u64 = 100_000;

pub const fn dyn_gas_limit(len: u64) -> u64 {
    FIXED_GAS + len * COLD_SSTORE_GAS
}

pub fn spend_nullifiers_tx<DB, I, Client, Pool>(
    ctx: &WorldChainPayloadBuilderCtx<Client, Pool>,
    evm: &mut OpEvm<DB, I>,
    nullifier_hashes: HashSet<Field>,
) -> eyre::Result<Recovered<OpTransactionSigned>>
where
    I: Inspector<OpContext<DB>>,
    DB: revm::Database + revm::DatabaseCommit,
    <DB as revm::Database>::Error: std::fmt::Debug + Send + Sync + derive_more::Error + 'static,
{
    let nonce = evm
        .db_mut()
        .basic(ctx.builder_private_key.address())?
        .unwrap_or_default()
        .nonce;

    let mut tx = OpTransactionRequest::default()
        .nonce(nonce)
        .gas_limit(dyn_gas_limit(nullifier_hashes.len() as u64))
        .max_priority_fee_per_gas(evm.ctx().block.basefee.into())
        .max_fee_per_gas(evm.ctx().block.basefee.into())
        .with_chain_id(evm.ctx().cfg.chain_id)
        .with_call(&spendNullifierHashesCall {
            _nullifierHashes: nullifier_hashes.into_iter().collect(),
        })
        .to(ctx.pbh_entry_point)
        .build_typed_tx()
        .map_err(|e| eyre!("{:?}", e))?;

    let signature = ctx.builder_private_key.sign_transaction_sync(&mut tx)?;
    let signed: OpTransactionSigned = tx.into_signed(signature).into();
    Ok(signed.into_recovered_unchecked()?)
}
