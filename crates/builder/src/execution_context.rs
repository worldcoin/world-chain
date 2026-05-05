use crate::{
    payload_builder_metrics::{PayloadBuildRejectionReason, PayloadBuildTaskOutcome},
    state_db::StateDB,
    traits::{context::PayloadBuilderCtx, context_builder::PayloadBuilderCtxBuilder},
    utils::estimated_da_size_bytes,
};
use alloy_consensus::{Block, SignableTransaction, Transaction, transaction::SignerRecoverable};
use alloy_eips::{Encodable2718, Typed2718};
use alloy_network::{TransactionBuilder, TxSignerSync};
use alloy_primitives::{Address, U256};
use alloy_signer_local::PrivateKeySigner;
use eyre::eyre::eyre;
use op_alloy_consensus::EIP1559ParamError;
use op_alloy_rpc_types::OpTransactionRequest;

use alloy_rpc_types_engine::PayloadId;
use op_revm::{L1BlockInfo, OpSpecId, constants::L1_BLOCK_CONTRACT};
use reth_basic_payload_builder::PayloadConfig;
use reth_evm::{
    ConfigureEvm, Database as RethDatabase, Evm, EvmEnv,
    block::{BlockExecutionError, BlockValidationError},
    execute::{BlockBuilder, BlockExecutor},
};
use reth_node_api::{NodePrimitives, PayloadBuilderError};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{
    OpBuiltPayload, OpEvmConfig, OpNextBlockEnvAttributes, OpPayloadBuilderAttributes,
    txpool::estimated_da_size::DataAvailabilitySized,
};
use reth_optimism_payload_builder::{
    builder::{ExecutionInfo, OpPayloadBuilderCtx},
    config::OpBuilderConfig,
};
use reth_optimism_primitives::OpTransactionSigned;
use reth_payload_primitives::BuildNextEnv;
use reth_payload_util::PayloadTransactions;
use reth_primitives_traits::{Recovered, SealedHeader, TxTy};
use reth_provider::{BlockReaderIdExt, ChainSpecProvider, StateProviderFactory};
use reth_revm::cancelled::CancelOnDrop;
use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction, TransactionPool};
use revm::{Database as RevmDatabase, DatabaseCommit, context::BlockEnv};
use revm_database::State;
use semaphore_rs::Field;
use std::{collections::HashSet, fmt::Debug, sync::Arc, time::Instant};
use tracing::{error, trace};
use world_chain_pool::{
    bindings::IPBHEntryPoint::spendNullifierHashesCall,
    tx::{WorldChainPoolTransaction, WorldChainPooledTransaction},
};

/// Container type that holds all necessities to build a new payload.
#[derive(Debug, Clone)]
pub struct WorldChainPayloadBuilderCtx<Client: ChainSpecProvider> {
    pub inner: Arc<OpPayloadBuilderCtx<OpEvmConfig, <Client as ChainSpecProvider>::ChainSpec>>,
    pub verified_blockspace_capacity: u8,
    pub pbh_entry_point: Address,
    pub pbh_signature_aggregator: Address,
    pub client: Client,
    pub builder_private_key: PrivateKeySigner,
    pub block_uncompressed_size_limit: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct WorldChainPayloadBuilderCtxBuilder {
    pub verified_blockspace_capacity: u8,
    pub pbh_entry_point: Address,
    pub pbh_signature_aggregator: Address,
    pub builder_private_key: PrivateKeySigner,
    pub block_uncompressed_size_limit: Option<u64>,
}

impl<Client> WorldChainPayloadBuilderCtx<Client>
where
    Client: StateProviderFactory
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
        + ChainSpecProvider<ChainSpec: OpHardforks>
        + Clone,
{
    /// After computing the execution result and state we can commit changes to the database
    fn commit_changes(
        &self,
        info: &mut ExecutionInfo,
        base_fee: u64,
        gas_used: u64,
        tx_da_size: u64,
        tx: Recovered<OpTransactionSigned>,
    ) {
        // add gas used by the transaction to cumulative gas used, before creating the
        // receipt
        info.cumulative_gas_used += gas_used;
        info.cumulative_da_bytes_used += tx_da_size;

        // update add to total fees
        let miner_fee = tx
            .effective_tip_per_gas(base_fee)
            .expect("fee is always valid; execution succeeded");
        info.total_fees += U256::from(miner_fee) * U256::from(gas_used);
    }
}

impl<Client> PayloadBuilderCtx for WorldChainPayloadBuilderCtx<Client>
where
    Client: StateProviderFactory
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
        + ChainSpecProvider<ChainSpec: OpHardforks>
        + Clone,
{
    type Evm = OpEvmConfig;
    type ChainSpec = <Client as ChainSpecProvider>::ChainSpec;
    type Transaction = WorldChainPooledTransaction;

    fn evm_config(&self) -> &Self::Evm {
        &self.inner.evm_config
    }

    fn spec(&self) -> &Self::ChainSpec {
        // TODO: Replace this is `self.inner.spec()` once PayloadBuilderCtx is implemented for
        // inner
        self.inner.chain_spec.as_ref()
    }

    fn builder_config(&self) -> &OpBuilderConfig {
        &self.inner.builder_config
    }

    fn evm_env(&self) -> Result<EvmEnv<OpSpecId>, EIP1559ParamError> {
        self.inner.evm_config.evm_env(self.parent())
    }

    fn parent(&self) -> &SealedHeader {
        self.inner.parent()
    }

    fn attributes(
        &self,
    ) -> &OpPayloadBuilderAttributes<TxTy<<Self::Evm as ConfigureEvm>::Primitives>> {
        self.inner.attributes()
    }

    fn best_transaction_attributes(
        &self,
        block_env: &revm::context::BlockEnv,
    ) -> BestTransactionsAttributes {
        self.inner.best_transaction_attributes(block_env)
    }

    fn payload_id(&self) -> PayloadId {
        self.inner.payload_id()
    }

    fn is_better_payload(&self, total_fees: U256) -> bool {
        self.inner.is_better_payload(total_fees)
    }

    fn block_builder<'a, DB>(
        &'a self,
        db: &'a mut State<DB>,
    ) -> Result<
        impl BlockBuilder<
            Executor: BlockExecutor<Evm: Evm<DB = &'a mut State<DB>, BlockEnv = BlockEnv>>,
            Primitives = <Self::Evm as ConfigureEvm>::Primitives,
        > + 'a,
        PayloadBuilderError,
    >
    where
        DB::Error: Send + Sync + 'static,
        DB: RethDatabase + 'a,
    {
        let attributes = OpNextBlockEnvAttributes::build_next_env(
            self.inner.attributes(),
            self.inner.parent(),
            self.spec(),
        )?;

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
            .context_for_next_block(self.inner.parent(), attributes)
            .map_err(PayloadBuilderError::other)?;

        // Prepare block builder.
        Ok(self
            .inner
            .evm_config
            .create_block_builder(evm, self.inner.parent(), execution_ctx))
    }

    fn execute_sequencer_transactions(
        &self,
        builder: &mut impl BlockBuilder<
            Primitives = <Self::Evm as ConfigureEvm>::Primitives,
            Executor: BlockExecutor<Evm: Evm<DB: StateDB + DatabaseCommit + reth_evm::Database>>,
        >,
    ) -> Result<ExecutionInfo, PayloadBuilderError> {
        self.inner.execute_sequencer_transactions(builder)
    }

    /// Executes the given best transactions and updates the execution info.
    ///
    /// Returns `Ok(Some(())` if the job was cancelled.
    fn execute_best_transactions<Pool, Txs, Builder>(
        &self,
        pool: Pool,
        info: &mut ExecutionInfo,
        builder: &mut Builder,
        mut best_txs: Txs,
        attempt_metrics: &mut crate::payload_builder_metrics::PayloadBuildAttemptMetrics,
        mut effective_gas_limit: u64,
        mut cumulative_uncompressed_bytes: u64,
    ) -> Result<Option<()>, PayloadBuilderError>
    where
        Pool: TransactionPool,
        Builder: BlockBuilder<
                Primitives = <Self::Evm as ConfigureEvm>::Primitives,
                Executor: BlockExecutor<
                    Evm: Evm<
                        DB: StateDB + DatabaseCommit + reth_evm::Database,
                        BlockEnv = BlockEnv,
                    >,
                >,
            >,
        Txs: PayloadTransactions<
            Transaction: WorldChainPoolTransaction<Consensus = OpTransactionSigned>,
        >,
    {
        let block_da_limit = self.inner.builder_config.da_config.max_da_block_size();
        let tx_da_limit = self.inner.builder_config.da_config.max_da_tx_size();
        let base_fee = builder.evm_mut().block().basefee;
        let da_footprint_gas_scalar = if self
            .spec()
            .is_jovian_active_at_timestamp(self.attributes().timestamp)
        {
            let db = builder.evm_mut().db_mut();
            db.basic(L1_BLOCK_CONTRACT)
                .map_err(PayloadBuilderError::other)?;
            Some(
                L1BlockInfo::fetch_da_footprint_gas_scalar(db)
                    .map_err(PayloadBuilderError::other)?,
            )
        } else {
            None
        };
        let mut transactions_considered = 0;
        let mut transactions_executed = 0;
        let mut invalid_txs = vec![];
        let verified_gas_limit =
            (self.verified_blockspace_capacity as u64 * effective_gas_limit) / 100;

        let mut spent_nullifier_hashes = HashSet::new();
        while let Some(pooled_tx) = best_txs.next(()) {
            transactions_considered += 1;
            let tx_da_size = pooled_tx.estimated_da_size();
            let tx = pooled_tx.clone().into_consensus();
            let tx_uncompressed_size = tx.encode_2718_len() as u64;
            attempt_metrics.record_transaction_size_bytes(tx_uncompressed_size);
            attempt_metrics.record_transaction_da_size_bytes(tx_da_size);
            cumulative_uncompressed_bytes += tx_uncompressed_size;
            let is_uncompressed_block_full =
                if let Some(block_uncompressed_size_limit) = self.block_uncompressed_size_limit {
                    let result = cumulative_uncompressed_bytes > block_uncompressed_size_limit;
                    if result {
                        tracing::warn!(
                            "we've reached block uncompressed size limit - rejecting tx: {:?}",
                            tx
                        );
                    }
                    result
                } else {
                    false
                };

            if is_uncompressed_block_full {
                attempt_metrics.increment_rejection(PayloadBuildRejectionReason::UncompressedSize);
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            if info.is_tx_over_limits(
                tx_da_size,
                effective_gas_limit,
                tx_da_limit,
                block_da_limit,
                tx.gas_limit(),
                da_footprint_gas_scalar,
            ) {
                attempt_metrics.increment_rejection(PayloadBuildRejectionReason::OverLimits);
                // we can't fit this transaction into the block, so we need to mark it as
                // invalid which also removes all dependent transaction from
                // the iterator before we can continue
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            // A sequencer's block should never contain blob or deposit transactions from the pool.
            if tx.is_eip4844() || tx.is_deposit() {
                attempt_metrics.increment_rejection(PayloadBuildRejectionReason::BlobOrDeposit);
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
                    attempt_metrics
                        .increment_rejection(PayloadBuildRejectionReason::VerifiedGasLimit);
                    best_txs.mark_invalid(tx.signer(), tx.nonce());
                    continue;
                }

                if payloads
                    .iter()
                    .any(|payload| !spent_nullifier_hashes.insert(payload.nullifier_hash))
                {
                    attempt_metrics
                        .increment_rejection(PayloadBuildRejectionReason::DuplicateNullifier);
                    best_txs.mark_invalid(tx.signer(), tx.nonce());
                    invalid_txs.push(*pooled_tx.hash());
                    continue;
                }
            }

            let tx_execution_started = Instant::now();
            let execution_result = builder.execute_transaction(tx.clone());
            attempt_metrics.record_transaction_execution_duration(tx_execution_started.elapsed());
            let gas_used = match execution_result {
                Ok(res) => {
                    if let Some(payloads) = pooled_tx.pbh_payload() {
                        if spent_nullifier_hashes.len() == payloads.len() {
                            effective_gas_limit -= FIXED_GAS
                        }

                        effective_gas_limit -= COLD_SSTORE_GAS * payloads.len() as u64;
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
                                attempt_metrics
                                    .increment_rejection(PayloadBuildRejectionReason::NonceTooLow);
                                // if the nonce is too low, we can skip this transaction
                                trace!(target: "payload_builder", %error, ?tx, "skipping nonce too low transaction");
                            } else {
                                attempt_metrics.increment_rejection(
                                    PayloadBuildRejectionReason::InvalidDescendant,
                                );
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

            attempt_metrics.record_transaction_gas_used(gas_used);
            transactions_executed += 1;
            self.commit_changes(info, base_fee, gas_used, tx_da_size, tx);
        }

        if !spent_nullifier_hashes.is_empty() {
            let spend_nullifiers_started = Instant::now();
            attempt_metrics.increment_spend_nullifiers_attempts();
            attempt_metrics.record_spend_nullifiers_count(spent_nullifier_hashes.len() as u64);
            let tx = spend_nullifiers_tx(self, builder.evm_mut(), spent_nullifier_hashes).map_err(
                |e| {
                    attempt_metrics
                        .record_spend_nullifiers_duration(spend_nullifiers_started.elapsed());
                    attempt_metrics
                        .record_spend_nullifiers_outcome(PayloadBuildTaskOutcome::Failure);
                    error!(target: "payload_builder", %e, "failed to build spend nullifiers transaction");
                    PayloadBuilderError::Other(e.into())
                },
            )?;

            // Try to execute the builder tx. In the event that execution fails due to
            // insufficient funds, continue with the built payload. This ensures that
            // PBH transactions still receive priority inclusion, even if the PBH nullifier
            // is not spent rather than sitting in the default execution client's mempool.
            match builder.execute_transaction(tx.clone()) {
                Ok(gas_used) => {
                    let tx_da_size = estimated_da_size_bytes(&tx);
                    self.commit_changes(info, base_fee, gas_used, tx_da_size, tx);
                    attempt_metrics
                        .record_spend_nullifiers_outcome(PayloadBuildTaskOutcome::Success);
                }
                Err(e) => {
                    attempt_metrics
                        .record_spend_nullifiers_outcome(PayloadBuildTaskOutcome::Failure);
                    error!(target: "payload_builder", %e, "spend nullifiers transaction failed")
                }
            }
            attempt_metrics.record_spend_nullifiers_duration(spend_nullifiers_started.elapsed());
        }

        let invalid_transactions_removed = invalid_txs.len() as u64;
        if !invalid_txs.is_empty() {
            pool.remove_transactions(invalid_txs);
        }

        attempt_metrics.record_transactions_considered_per_build(transactions_considered);
        attempt_metrics.record_transactions_executed_per_build(transactions_executed);
        attempt_metrics.record_invalid_transactions_removed_per_build(invalid_transactions_removed);

        Ok(None)
    }
}

impl<Provider> PayloadBuilderCtxBuilder<Provider, OpEvmConfig, OpChainSpec>
    for WorldChainPayloadBuilderCtxBuilder
where
    Provider: StateProviderFactory
        + ChainSpecProvider<ChainSpec = OpChainSpec>
        + Send
        + Sync
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
        + Clone,
{
    type PayloadBuilderCtx = WorldChainPayloadBuilderCtx<Provider>;

    fn build(
        &self,
        provider: Provider,
        evm_config: OpEvmConfig,
        builder_config: OpBuilderConfig,
        config: PayloadConfig<
            OpPayloadBuilderAttributes<
                <<OpEvmConfig as ConfigureEvm>::Primitives as NodePrimitives>::SignedTx,
            >,
            <<OpEvmConfig as ConfigureEvm>::Primitives as NodePrimitives>::BlockHeader,
        >,
        cancel: &CancelOnDrop,
        best_payload: Option<OpBuiltPayload<<OpEvmConfig as ConfigureEvm>::Primitives>>,
    ) -> Self::PayloadBuilderCtx
    where
        Self: Sized,
    {
        let inner = OpPayloadBuilderCtx {
            evm_config,
            builder_config,
            chain_spec: provider.chain_spec(),
            config,
            cancel: cancel.clone(),
            best_payload,
        };

        WorldChainPayloadBuilderCtx {
            inner: Arc::new(inner),
            client: provider.clone(),
            verified_blockspace_capacity: self.verified_blockspace_capacity,
            pbh_entry_point: self.pbh_entry_point,
            pbh_signature_aggregator: self.pbh_signature_aggregator,
            builder_private_key: self.builder_private_key.clone(),
            block_uncompressed_size_limit: self.block_uncompressed_size_limit,
        }
    }
}

pub const COLD_SSTORE_GAS: u64 = 20000;
pub const FIXED_GAS: u64 = 100_000;

pub const fn dyn_gas_limit(len: u64) -> u64 {
    FIXED_GAS + len * COLD_SSTORE_GAS
}

pub fn spend_nullifiers_tx<DB, EVM, Client>(
    ctx: &WorldChainPayloadBuilderCtx<Client>,
    evm: &mut EVM,
    nullifier_hashes: HashSet<Field>,
) -> eyre::Result<Recovered<OpTransactionSigned>>
where
    Client: StateProviderFactory
        + ChainSpecProvider<ChainSpec: OpHardforks>
        + Send
        + Sync
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
        + Clone,
    EVM: Evm<DB = DB, BlockEnv = BlockEnv>,
    DB: revm::Database,
    <DB as revm::Database>::Error: Send + Sync + 'static,
{
    let nonce = evm
        .db_mut()
        .basic(ctx.builder_private_key.address())?
        .unwrap_or_default()
        .nonce;

    let mut tx = OpTransactionRequest::default()
        .nonce(nonce)
        .gas_limit(dyn_gas_limit(nullifier_hashes.len() as u64))
        .max_priority_fee_per_gas(evm.block().basefee.into())
        .max_fee_per_gas(evm.block().basefee.into())
        .with_chain_id(evm.chain_id())
        .with_call(&spendNullifierHashesCall {
            _nullifierHashes: nullifier_hashes.into_iter().collect(),
        })
        .to(ctx.pbh_entry_point)
        .build_typed_tx()
        .map_err(|e| eyre!("{:?}", e))?;

    let signature = ctx.builder_private_key.sign_transaction_sync(&mut tx)?;
    let signed: OpTransactionSigned = tx.into_signed(signature).into();
    Ok(signed.try_into_recovered_unchecked()?)
}
